use log::{debug, info, trace, warn};
use mqtt::control::variable_header::ConnectReturnCode;
use mqtt::encodable::Decodable;
use mqtt::encodable::Encodable;
use mqtt::packet::{
    ConnackPacket, ConnectPacket, DisconnectPacket, Packet, PingreqPacket, PingrespPacket,
    SubscribePacket, VariablePacket,
};
use mqtt::QualityOfService;
use mqtt::TopicFilter;

use std::io::{Cursor, Error, ErrorKind};
use std::net::Shutdown;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{tcp::WriteHalf, TcpStream};
use tokio::select;
use tokio::signal::unix::{signal, SignalKind};
use tokio::stream::StreamExt;
use tokio::time::{delay_for, interval};

mod serial;
use serial::Switcher;

static MQTT_ADDR: &str = "beefy.sigmaris.info:1883";
const KEEP_ALIVE: u16 = 50;

#[cfg(feature = "log_to_syslog")]
fn log_init() -> Result<(), Box<dyn std::error::Error>> {
    syslog::init(syslog::Facility::LOG_LOCAL5, log::LevelFilter::Info, None)?;
    debug!("Logging to syslog");
    Ok(())
}

#[cfg(not(feature = "log_to_syslog"))]
fn log_init() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    debug!("Logging to stderr");
    Ok(())
}

async fn connect() -> Result<TcpStream, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(MQTT_ADDR).await?;
    debug!("Connected TcpStream");
    let mut cp = ConnectPacket::new("MQTT", "avcontrol-rs");
    cp.set_clean_session(true);
    cp.set_keep_alive(KEEP_ALIVE);

    trace!(">M CONNECT {:?}", cp);

    let mut buf = Vec::new();
    cp.encode(&mut buf).unwrap();
    stream.write_all(&buf[..]).await?;
    let mut in_buf = [0; 4];
    stream.read_exact(&mut in_buf).await?;
    let connack = ConnackPacket::decode(&mut Cursor::new(&mut in_buf))?;
    trace!("<M CONNACK {:?}", connack);
    debug!("Connection acknowledged");
    if connack.connect_return_code() != ConnectReturnCode::ConnectionAccepted {
        Err(Box::new(Error::new(
            ErrorKind::Other,
            format!(
                "Connection failure, acknowledgement return code {:?}",
                connack.connect_return_code()
            ),
        )))
    } else {
        Ok(stream)
    }
}

async fn handle_packet(
    packet: &VariablePacket,
    writer: &mut WriteHalf<'_>,
    switcher: &mut Switcher,
) -> Result<(), Box<dyn std::error::Error>> {
    match packet {
        VariablePacket::PingrespPacket(..) => {
            debug!("Received PINGRESP from broker");
        }
        VariablePacket::PingreqPacket(..) => {
            debug!("Sending ping response to broker");
            let pingresp = PingrespPacket::new();
            trace!(">M PINGRESP {:?}", pingresp);
            let mut buf = Vec::new();
            pingresp.encode(&mut buf).unwrap();
            writer.write_all(&buf).await?
        }
        VariablePacket::PublishPacket(ref publ) => {
            debug!(
                "Received on topic {}: {:?}",
                publ.topic_name(),
                publ.payload_ref()
            );
            if let Ok(input_name) = std::str::from_utf8(publ.payload_ref()) {
                switcher.switch_to(input_name).await?;
            } else {
                warn!("{:?} is invalid UTF-8", publ.payload_ref())
            }
        }
        foo => debug!("Unhandled packet: {:?}", foo),
    }
    Ok(())
}

async fn send_disconnect(writer: &mut WriteHalf<'_>) {
    let disconn = DisconnectPacket::new();
    trace!(">M DISCONNECT {:?}", disconn);
    let mut buf = Vec::new();
    disconn.encode(&mut buf).unwrap();
    writer.write_all(&buf).await.ok();
}

async fn main_loop(
    mut stream: TcpStream,
    sigint: &mut tokio::signal::unix::Signal,
    sigterm: &mut tokio::signal::unix::Signal,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut switcher = serial::Switcher::new()?;
    let (mut mqtt_read, mut mqtt_write) = stream.split();
    let packet_id: u16 = rand::random();
    let sub = SubscribePacket::new(
        packet_id,
        vec![(
            TopicFilter::new("avcontrol/switchto").unwrap(),
            QualityOfService::Level0,
        )],
    );
    trace!(">M SUBSCRIBE {:?}", sub);
    let mut buf = Vec::new();
    sub.encode(&mut buf)?;
    mqtt_write.write_all(&buf).await?;

    let ping_time = Duration::from_secs((KEEP_ALIVE / 2) as u64);
    let mut ping_stream = interval(ping_time);
    loop {
        select! {
            Some(_) = ping_stream.next() => {
                debug!("Sending PINGREQ to broker");
                let pingreq = PingreqPacket::new();
                trace!(">M PINGREQ {:?}", pingreq);
                let mut buf = Vec::new();
                pingreq.encode(&mut buf).unwrap();
                mqtt_write.write_all(&buf).await?
            }
            Some(_) = sigint.next() => {
                debug!("Caught SIGINT");
                send_disconnect(&mut mqtt_write).await;
                return Ok(());
            }
            Some(_) = sigterm.next() => {
                debug!("Caught SIGTERM");
                send_disconnect(&mut mqtt_write).await;
                return Ok(());
            }
            Ok(packet) = VariablePacket::parse(&mut mqtt_read) => {
                trace!("<M PACKET {:?}", packet);
                handle_packet(&packet, &mut mqtt_write, &mut switcher).await?;
            }
            else => break
        }
    }
    stream.shutdown(Shutdown::Both).ok();
    Ok(())
}

#[tokio::main]
async fn main() {
    log_init().expect("Failed logger init");
    let mut sigint = signal(SignalKind::interrupt()).expect("Can't listen for SIGINT");
    let mut sigterm = signal(SignalKind::terminate()).expect("Can't listen for SIGTERM");
    loop {
        let mut result = connect().await;
        while result.is_err() {
            warn!("Connect failed, retrying: {:?}", result);
            delay_for(Duration::from_secs(2)).await;
            result = connect().await;
        }
        let ended = main_loop(result.unwrap(), &mut sigint, &mut sigterm).await;
        if ended.is_err() {
            info!("Main loop ended, reconnecting: {:?}", ended);
            delay_for(Duration::from_secs(1)).await;
        } else {
            break;
        }
    }
}
