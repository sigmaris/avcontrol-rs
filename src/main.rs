use futures::future::{BoxFuture, FutureExt};
use log::{debug, info, trace, warn};
use mqtt::control::variable_header::ConnectReturnCode;
use mqtt::encodable::{Decodable, Encodable};
use mqtt::packet::{
    publish::QoSWithPacketIdentifier, ConnackPacket, ConnectPacket, DisconnectPacket,
    PingreqPacket, PingrespPacket, PublishPacket, SubscribePacket, VariablePacket,
};
use mqtt::{QualityOfService, TopicFilter, TopicName};
use serde::Serialize;
use std::io::{Cursor, Error, ErrorKind};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{tcp::WriteHalf, TcpStream};
use tokio::select;
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{interval, sleep};

mod serial;
use serial::get_switch_options;
use serial::Switcher;

static DISCOVERY_TOPIC: &str = "homeassistant/select/living_room_input/config";
static OBJECT_ID: &str = "living_room_input";
static AVAIL_TOPIC: &str = "avcontrol/availability";
static SWITCH_TOPIC: &str = "avcontrol/switchto";
static STATE_TOPIC: &str = "avcontrol/state";
static TV_POWER_STATE_TOPIC: &str = "avcontrol/tv_power_state";
static TV_POWER_COMMAND_TOPIC: &str = "avcontrol/tv_power_set";

#[derive(Serialize)]
struct Availability<'a> {
    topic: &'a str,
}

#[derive(Serialize)]
struct DiscoveryConfig<'a> {
    availability: Availability<'a>,
    command_topic: &'a str,
    icon: &'a str,
    name: &'a str,
    object_id: &'a str,
    options: Vec<&'a str>,
    retain: bool,
    state_topic: &'a str,
    unique_id: &'a str,
}

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
    let mut stream = TcpStream::connect(std::env::var("MQTT_ADDR")?).await?;
    debug!("Connected TcpStream");
    let mut cp = ConnectPacket::new("avcontrol-rs");
    cp.set_clean_session(true);
    cp.set_keep_alive(KEEP_ALIVE);
    // Send a last will availability message when we go offline
    cp.set_will(Some((
        TopicName::new(AVAIL_TOPIC).unwrap(),
        "offline".into(),
    )));
    cp.set_will_retain(true);

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
    power: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    publish(
        writer,
        TV_POWER_STATE_TOPIC,
        if power { "ON" } else { "OFF" },
        true,
    )
    .await
}

async fn publish_disco(writer: &mut WriteHalf<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let disco = DiscoveryConfig {
        availability: Availability { topic: AVAIL_TOPIC },
        command_topic: SWITCH_TOPIC,
        icon: "mdi:television",
        name: "TV Input",
        object_id: OBJECT_ID,
        options: get_switch_options(),
        retain: false,
        state_topic: STATE_TOPIC,
        unique_id: OBJECT_ID,
    };
    publish(
        writer,
        DISCOVERY_TOPIC,
        serde_json::to_vec(&disco).unwrap(),
        true,
    )
    .await
}

async fn publish<P>(
    writer: &mut WriteHalf<'_>,
    topic: &str,
    payload: P,
    retain: bool,
) -> Result<(), Box<dyn std::error::Error>>
where
    P: Into<Vec<u8>>,
{
    let mut status_pkt = PublishPacket::new(
        TopicName::new(topic).unwrap(),
        QoSWithPacketIdentifier::Level0,
        payload,
    );
    status_pkt.set_retain(retain);

    trace!("<M PUBLISH {:?}", status_pkt);

    let mut buf = Vec::new();
    status_pkt.encode(&mut buf)?;
    writer.write_all(&buf).await?;
    Ok(())
}

async fn send_disconnect(writer: &mut WriteHalf<'_>) {
    publish(writer, AVAIL_TOPIC, "offline", true).await.ok();

    let disconn = DisconnectPacket::new();
    trace!(">M DISCONNECT {:?}", disconn);
    let mut buf = Vec::new();
    disconn.encode(&mut buf).unwrap();
    writer.write_all(&buf).await.ok();
}

struct Controller {
    switcher: Switcher,
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
                    publ.payload()
                );
                if SWITCH_TOPIC == publ.topic_name() {
                    // Switch AV input
                    if let Ok(input_name) = std::str::from_utf8(publ.payload()) {
                        self.switch_to_input(input_name, writer).await?;
                    } else {
                        warn!("{:?} is invalid UTF-8", publ.payload());
                    }
                } else {
                    warn!("Unhandled topic {}", publ.topic_name());
                }
            }
            other => debug!("Unhandled packet: {:?}", other),
        }
        Ok(())
    }

    fn switch_to_input<'a>(
        &'a mut self,
        input_name: &'a str,
        writer: &'a mut WriteHalf<'_>,
    ) -> BoxFuture<'a, Result<(), Box<dyn std::error::Error>>> {
        async move {
            let mut tv_power_on = false;
            match self.switcher.switch_to(input_name).await {
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
                    publish(writer, STATE_TOPIC, input_name, true).await?;
                }
            }
            publish_tv_status(writer, tv_power_on).await?;
            Ok(())
        }
        .boxed()
    }

    async fn main_loop(&mut self, mut stream: TcpStream) -> Result<(), Box<dyn std::error::Error>> {
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
                    TopicFilter::new(TV_POWER_COMMAND_TOPIC).unwrap(),
                    QualityOfService::Level0,
                ),
            ],
        );
        trace!(">M SUBSCRIBE {:?}", sub);
        let mut buf = Vec::new();
        sub.encode(&mut buf)?;
        mqtt_write.write_all(&buf).await?;

        publish_disco(&mut mqtt_write).await?;
        publish(&mut mqtt_write, AVAIL_TOPIC, "online", true).await?;
        publish_tv_status(&mut mqtt_write, self.switcher.tv_power_status(true).await?).await?;

        let ping_time = Duration::from_secs((KEEP_ALIVE / 2) as u64);
        let mut ping_stream = interval(ping_time);
        loop {
            select! {
                _ = ping_stream.tick() => {
                    debug!("Sending PINGREQ to broker");
                    let pingreq = PingreqPacket::new();
                    trace!(">M PINGREQ {:?}", pingreq);
                    let mut buf = Vec::new();
                    pingreq.encode(&mut buf).unwrap();
                    mqtt_write.write_all(&buf).await?
                }
                Some(_) = self.sigint.recv() => {
                    debug!("Caught SIGINT");
                    send_disconnect(&mut mqtt_write).await;
                    return Ok(());
                }
                Some(_) = self.sigterm.recv() => {
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
        stream.shutdown().await?;
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    log_init().expect("Failed logger init");
    let mut controller = Controller {
        switcher: serial::Switcher::new().expect("Couldn't open serial ports"),
        sigint: signal(SignalKind::interrupt()).expect("Can't listen for SIGINT"),
        sigterm: signal(SignalKind::terminate()).expect("Can't listen for SIGTERM"),
    };
    loop {
        let mut result = connect().await;
        while result.is_err() {
            warn!("MQTT Connect failed, retrying: {:?}", result);
            sleep(Duration::from_secs(2)).await;
            result = connect().await;
        }
        let ended = controller.main_loop(result.unwrap()).await;
        if ended.is_err() {
            info!("Main loop ended, reconnecting: {:?}", ended);
            sleep(Duration::from_secs(1)).await;
        } else {
            break;
        }
    }
}
