use async_tungstenite::tokio::connect_async;
use clap::{App, Arg, ArgMatches, SubCommand};
use futures::sink::SinkExt;
use futures::{stream::StreamExt, Future, Sink, Stream};
use log::{info, LevelFilter};
use simplelog::{CombinedLogger, TermLogger, TerminalMode};
use std::time::Duration;
use tab_api::{
    chunk::{Chunk, ChunkType, StdinChunk},
    config::load_daemon_file,
    request::Request,
    response::Response,
    tab::{CreateTabMetadata, TabId},
};
use tab_websocket::{decode, decode_with, encode_with};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::{process::Command, time::delay_for};
use tungstenite::Message;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Starting.");

    let matches = init();
    run().await?;

    info!("Complete.  Shutting down");
    Ok(())
}

fn init() -> ArgMatches<'static> {
    CombinedLogger::init(vec![TermLogger::new(
        LevelFilter::Debug,
        simplelog::Config::default(),
        TerminalMode::Stderr,
    )])
    .unwrap();

    App::new("Terminal Multiplexer")
        .version("0.1")
        .author("Austin Jones <implAustin@gmail.com>")
        .about("Provides persistent terminal sessions with multiplexing.")
        .arg(
            Arg::with_name("TAB")
                .help("Switches to the provided tab")
                .required(false)
                .index(1),
        )
        .arg(
            Arg::with_name("command")
                .short("c")
                .possible_values(&["list", "_autocomplete-tab"])
                .help("print debug information verbosely"),
        )
        .get_matches()
}

async fn run() -> anyhow::Result<()> {
    info!("Loading daemon file");
    let daemon_file = load_daemon_file()?;
    if daemon_file.is_none() {
        info!("Starting daemon");
        start_daemon().await?;
    }

    while let None = load_daemon_file()? {
        delay_for(Duration::from_millis(25)).await;
    }

    info!("Connecting WebSocket");
    let daemon_file = load_daemon_file()?.expect("daemon file should be present");
    let server_url = format!("ws://127.0.0.1:{}", daemon_file.port);
    let (websocket, _) = connect_async(server_url).await?;

    let (tx, rx) = websocket.split();
    let tx = tx.with(|msg| encode_with(msg));
    let rx = rx.map(|msg| decode_with::<Response>(msg));
    tokio::spawn(send_loop(tx));

    recv_loop(rx).await?;

    Ok(())
}

async fn send_loop(
    mut tx: impl Sink<Request, Error = anyhow::Error> + Unpin,
) -> anyhow::Result<()> {
    tx.send(Request::Auth(vec![])).await?;
    tx.send(Request::ListTabs).await?;
    tx.send(Request::CreateTab(CreateTabMetadata {
        name: "foo".to_string(),
    }))
    .await?;

    forward_stdin(tx).await?;

    Ok(())
}

async fn forward_stdin(
    mut tx: impl Sink<Request, Error = anyhow::Error> + Unpin,
) -> anyhow::Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut buffer = vec![0u8; 512];

    while let Ok(read) = stdin.read(buffer.as_mut_slice()).await {
        if read == 0 {
            continue;
        }

        let mut buf = vec![0; read];
        buf.copy_from_slice(&buffer[0..read]);

        let chunk = StdinChunk { data: buf };
        // TODO: use selected tab
        tx.send(Request::Stdin(TabId(0), chunk)).await?;
    }

    Ok(())
}

async fn recv_loop(
    mut rx: impl Stream<Item = impl Future<Output = anyhow::Result<Response>>> + Unpin,
) -> anyhow::Result<()> {
    info!("Waiting on messages...");

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();

    while let Some(message) = rx.next().await {
        let message = message.await?;
        // info!("message: {:?}", message);

        match message {
            Response::Chunk(tab_id, chunk) => match chunk.channel {
                ChunkType::Stdout => {
                    stdout.write_all(chunk.data.as_slice()).await?;
                }
                ChunkType::Stderr => {
                    stderr.write_all(chunk.data.as_slice()).await?;
                }
            },
            Response::TabUpdate(tab) => {}
            Response::TabList(tabs) => {}
        }
    }

    Ok(())
}

async fn start_daemon() -> anyhow::Result<()> {
    Command::new("tab-daemon").spawn()?.await?;
    Ok(())
}
