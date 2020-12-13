use log::{debug, info, trace, warn};
use futures::future::{BoxFuture, FutureExt};
use mqtt::control::variable_header::ConnectReturnCode;
use mqtt::encodable::{Decodable, Encodable};
use mqtt::packet::{
    ConnackPacket, ConnectPacket, DisconnectPacket, Packet, PingreqPacket, PingrespPacket,
    SubscribePacket, VariablePacket, PublishPacket, publish::QoSWithPacketIdentifier
};
use mqtt::{QualityOfService, TopicFilter, TopicName};
use std::io::{Cursor, Error, ErrorKind};
use std::net::Shutdown;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{tcp::WriteHalf, TcpStream};
use tokio::select;
use tokio::signal::unix::{signal, SignalKind};
use tokio::stream::StreamExt;
use tokio::time::{delay_for, interval};

mod lirc;
use lirc::LircSender;
mod serial;
use serial::Switcher;

static MQTT_ADDR: &str = "beefy.sigmaris.info:1883";
static SWITCH_TOPIC: &str = "avcontrol/switchto";
static TV_POWER_TOPIC: &str = "avcontrol/tvpower";
const KEEP_ALIVE: u16 = 50;

#[cfg(feature = "log_to_syslog")]
fn log_init() -> Result<(), Box<dyn std::error::Error>> {
    let level_filter = if let Ok(ref val) = std::env::var("RUST_LOG") {
        match val.as_str() {
            "OFF" => log::LevelFilter::Off,
            "ERROR" => log::LevelFilter::Error,
            "WARN" => log::LevelFilter::Warn,
            "INFO" => log::LevelFilter::Info,
            "DEBUG" => log::LevelFilter::Debug,
            "TRACE" => log::LevelFilter::Trace,
            _other => log::LevelFilter::Info,
        }
    } else {
        log::LevelFilter::Info
    };
    syslog::init(syslog::Facility::LOG_LOCAL5, level_filter, None)?;
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

async fn publish_tv_status(
    writer: &mut WriteHalf<'_>,
    power: bool
) -> Result<(), Box<dyn std::error::Error>> {
    let mut status_pkt = PublishPacket::new(
        TopicName::new(TV_POWER_TOPIC).unwrap(),
        QoSWithPacketIdentifier::Level0,
        if power { "ON" } else { "OFF" }
    );
    status_pkt.set_retain(true);
    let mut buf = Vec::new();
    status_pkt.encode(&mut buf)?;
    writer.write_all(&buf).await?;
    Ok(())
}

async fn send_disconnect(writer: &mut WriteHalf<'_>) {
    let disconn = DisconnectPacket::new();
    trace!(">M DISCONNECT {:?}", disconn);
    let mut buf = Vec::new();
    disconn.encode(&mut buf).unwrap();
    writer.write_all(&buf).await.ok();
}

struct Controller {
    switcher: Switcher,
    lirc_sender: LircSender,
    sigint: tokio::signal::unix::Signal,
    sigterm: tokio::signal::unix::Signal,
}

impl Controller {
    async fn handle_packet(
        &mut self,
        packet: &VariablePacket,
        writer: &mut WriteHalf<'_>,
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
                if SWITCH_TOPIC == publ.topic_name() {
                    // Switch AV input
                    if let Ok(input_name) = std::str::from_utf8(publ.payload_ref()) {
                        self.switch_to_input(input_name, true, writer).await?;
                    } else {
                        warn!("{:?} is invalid UTF-8", publ.payload_ref());
                    }
                } else if TV_POWER_TOPIC == publ.topic_name() {
                    if publ.payload_ref() == b"ON" {
                        if self.set_tv_power(true).await? {
                            publish_tv_status(writer, true).await?;
                        }
                    } else if publ.payload_ref() == b"OFF" {
                        if self.set_tv_power(false).await? {
                            publish_tv_status(writer, false).await?;
                        }
                    } else {
                        warn!("Unhandled TV power payload {:?}", publ.payload_ref());
                    }
                } else {
                    warn!("Unhandled topic {}", publ.topic_name());
                }
            }
            other => debug!("Unhandled packet: {:?}", other),
        }
        Ok(())
    }

    async fn set_tv_power(
        &mut self,
        power_state: bool,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let power_state_text = if power_state { "on" } else { "off" };
        if self.switcher.tv_power_status().await? == power_state {
            debug!("TV power is already {}", power_state_text);
            Ok(false)
        } else {
            self.lirc_sender.send_power_key().await?;
            for delay in 0..10 {
                delay_for(Duration::from_millis(500)).await;
                if self.switcher.tv_power_status().await? == power_state {
                    return Ok(true);
                } else {
                    debug!("TV power unchanged after {} ms", delay * 500);
                }
            }
            Err(Box::new(std::io::Error::new(ErrorKind::Other, "Timed out waiting for TV to change power state")))
        }
    }

    fn switch_to_input<'a>(
        &'a mut self,
        input_name: &'a str,
        try_power_on: bool,
        writer: &'a mut WriteHalf<'_>,
    ) -> BoxFuture<'a, Result<(), Box<dyn std::error::Error>>> {
        async move {
            let mut tv_power_on = false;
            match self.switcher.switch_to(input_name).await {
                Err(_) if try_power_on => {
                    debug!("TV is probably powered off, try powering on");
                    self.set_tv_power(true).await?;
                    return self.switch_to_input(input_name, false, writer).await
                }
                Err(err) => {
                    warn!("Error switching to {}: {}", input_name, err);
                }
                Ok(state_changed) => {
                    if state_changed {
                        info!("Switched OK to {}", input_name);
                    } else {
                        debug!("Already switched to {}", input_name);
                    }
                    tv_power_on = true;
                }
            }
            publish_tv_status(writer, tv_power_on).await?;
            Ok(())
        }.boxed()
    }

    async fn main_loop(
        &mut self,
        mut stream: TcpStream,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (mut mqtt_read, mut mqtt_write) = stream.split();
        let packet_id: u16 = rand::random();
        let sub = SubscribePacket::new(
            packet_id,
            vec![
                (
                    TopicFilter::new(SWITCH_TOPIC).unwrap(),
                    QualityOfService::Level0,
                ),
                (
                    TopicFilter::new(TV_POWER_TOPIC).unwrap(),
                    QualityOfService::Level0,
                ),
            ],
        );
        trace!(">M SUBSCRIBE {:?}", sub);
        let mut buf = Vec::new();
        sub.encode(&mut buf)?;
        mqtt_write.write_all(&buf).await?;

        publish_tv_status(&mut mqtt_write, self.switcher.tv_power_status().await?).await?;

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
                Some(_) = self.sigint.next() => {
                    debug!("Caught SIGINT");
                    send_disconnect(&mut mqtt_write).await;
                    return Ok(());
                }
                Some(_) = self.sigterm.next() => {
                    debug!("Caught SIGTERM");
                    send_disconnect(&mut mqtt_write).await;
                    return Ok(());
                }
                Ok(packet) = VariablePacket::parse(&mut mqtt_read) => {
                    trace!("<M PACKET {:?}", packet);
                    self.handle_packet(&packet, &mut mqtt_write).await?;
                }
                else => break
            }
        }
        stream.shutdown(Shutdown::Both).ok();
        Ok(())
    }

}

#[tokio::main]
async fn main() {
    log_init().expect("Failed logger init");
    let mut controller = Controller {
        switcher: serial::Switcher::new().expect("Couldn't open serial ports"),
        lirc_sender: LircSender::find_device().await.expect("Couldn't open LIRC sender"),
        sigint: signal(SignalKind::interrupt()).expect("Can't listen for SIGINT"),
        sigterm: signal(SignalKind::terminate()).expect("Can't listen for SIGTERM"),
    };
    loop {
        let mut result = connect().await;
        while result.is_err() {
            warn!("MQTT Connect failed, retrying: {:?}", result);
            delay_for(Duration::from_secs(2)).await;
            result = connect().await;
        }
        let ended = controller.main_loop(result.unwrap()).await;
        if ended.is_err() {
            info!("Main loop ended, reconnecting: {:?}", ended);
            delay_for(Duration::from_secs(1)).await;
        } else {
            break;
        }
    }
}
