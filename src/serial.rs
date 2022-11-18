use log::{trace, warn};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use phf::phf_map;

use std::io::{Error, ErrorKind};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{sleep, timeout};
use tokio::try_join;
use tokio_serial::{
    ClearBuffer, DataBits, FlowControl, Parity, SerialPort, SerialPortBuilderExt, SerialStream,
    StopBits,
};

const COMP_SWITCH_DEV: &str = "/dev/compswitch";
const HDMI_SWITCH_DEV: &str = "/dev/hdmiswitch";
const TV_SWITCH_DEV: &str = "/dev/tvserial";
static SWITCH_OPTIONS: phf::Map<&'static str, SwitchOption> = phf_map! {
    "PS2" =>           SwitchOption { input: Some(CompOrHDMI::Component(ComponentInput::PS2)), tv: TVInput::Component, aspect: AspectRatio::FourToThree },
    "Wii" =>           SwitchOption { input: Some(CompOrHDMI::Component(ComponentInput::Wii)), tv: TVInput::Component, aspect: AspectRatio::SixteenToNine },
    "External" =>      SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::External)),      tv: TVInput::HDMI1,     aspect: AspectRatio::SixteenToNine },
    "Switch" =>        SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::Switch)),        tv: TVInput::HDMI1,     aspect: AspectRatio::SixteenToNine },
    "PS4" =>           SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::PS4)),           tv: TVInput::HDMI1,     aspect: AspectRatio::SixteenToNine },
    "Fire TV Stick" => SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::FireTVStick)),   tv: TVInput::HDMI1,     aspect: AspectRatio::SixteenToNine },
    "Dreamcast" =>     SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::Dreamcast)),     tv: TVInput::HDMI1,     aspect: AspectRatio::FourToThree },
    "Kodi" =>          SwitchOption { input: None, tv: TVInput::HDMI2, aspect: AspectRatio::SixteenToNine },
    "SCART" =>         SwitchOption { input: None, tv: TVInput::SCART, aspect: AspectRatio::FourToThree },
};

#[derive(Copy, Clone, IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
pub enum TVInput {
    Component = 0x300,
    HDMI1 = 0x500,
    HDMI2 = 0x501,
    SCART,
}

#[derive(Copy, Clone, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum ComponentInput {
    PS2 = 1,
    Wii = 2,
}

#[derive(Copy, Clone, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum HDMIInput {
    External = 0,
    Switch = 1,
    PS4 = 2,
    FireTVStick = 3,
    Dreamcast = 4,
}

#[derive(Copy, Clone)]
pub enum CompOrHDMI {
    Component(ComponentInput),
    HDMI(HDMIInput),
}

#[derive(Copy, Clone, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum AspectRatio {
    SixteenToNine = 0x00,
    FourToThree = 0x04,
}

pub struct SwitchOption {
    pub input: Option<CompOrHDMI>,
    pub tv: TVInput,
    pub aspect: AspectRatio,
}

#[derive(Debug, PartialEq)]
pub enum TVResponse {
    Ok,
    None,
    Unexpected([u8; 3]),
}

struct TVSwitch {
    port: SerialStream,
}

impl TVSwitch {
    pub async fn power_status(&mut self, any_response: bool) -> Result<bool, Error> {
        #[cfg(not(test))]
        self.port.clear(ClearBuffer::Input)?;

        let response = self.send_cmd(&[0x0d, 0x00, 0x00, 0x1f]).await?; // Info key
        if response == TVResponse::None {
            // Empty response indicates TV is Off
            Ok(false)
        } else if any_response {
            // Anything else indicates TV is on
            Ok(true)
        } else {
            Ok(response == TVResponse::Ok)
        }
    }

    pub async fn switch_to(&mut self, input: TVInput) -> Result<(), Error> {
        #[cfg(not(test))]
        self.port.clear(ClearBuffer::Input)?;

        let code: [u8; 4] = match input {
            TVInput::SCART => {
                [0x0d, 0x00, 0x00, 0x84] // KEY_AV1 (switches to SCART)
            }
            other => {
                let other_bytes: u16 = other.into();
                // Bytes 1&2 are always 0x0a 0x00, byte 3 is source and 4 is index (e.g. 0/1 for HDMI1/2)
                [
                    0x0a,
                    0x00,
                    ((other_bytes & 0xff00) >> 8) as u8,
                    (other_bytes & 0xff) as u8,
                ]
            }
        };
        self.send_and_handle(code).await?;

        Ok(())
    }

    async fn send_and_handle(&mut self, code: [u8; 4]) -> Result<(), Error> {
        Ok(match self.send_cmd(&code).await? {
            TVResponse::Ok => {}
            TVResponse::None => {
                return Err(Error::new(
                    ErrorKind::Other,
                    "No response from TV, probably powered off",
                ))
            }
            TVResponse::Unexpected(ref buf) => warn!("Unexpected response from TV: {:?}", buf),
        })
    }

    async fn set_aspect(&mut self, aspect: AspectRatio) -> Result<(), Error> {
        self.send_and_handle([0x0b, 0x0a, 0x01, aspect.into()])
            .await
    }

    async fn send_cmd(&mut self, cmd: &[u8; 4]) -> Result<TVResponse, Error> {
        let prefixed_cmd = [0x08, 0x22, cmd[0], cmd[1], cmd[2], cmd[3]];
        let sum: u32 = prefixed_cmd.iter().map(|x| *x as u32).sum();
        let checksum: u8 = ((256 - sum) % 256) as u8;

        trace!(">T {:x?}", prefixed_cmd);
        trace!(">T {:x?}", [checksum]);

        self.port.write_all(&prefixed_cmd).await?;
        self.port.write_u8(checksum).await?;

        let mut resp_buf = [0u8; 3];
        match timeout(
            Duration::from_millis(100),
            self.port.read_exact(&mut resp_buf),
        )
        .await
        {
            Ok(_) => {
                trace!("<T {:x?}", resp_buf);

                if resp_buf == [0x03, 0x0c, 0xf1] {
                    Ok(TVResponse::Ok)
                } else {
                    Ok(TVResponse::Unexpected(resp_buf))
                }
            }
            Err(_) => Ok(TVResponse::None),
        }
    }
}

struct ComponentSwitch {
    port: SerialStream,
}

impl ComponentSwitch {
    // Is power on, off, or unknown (error)?
    pub async fn power_status(&mut self) -> Result<bool, Error> {
        #[cfg(not(test))]
        self.port.clear(ClearBuffer::Input)?;

        trace!(">C {:x?}", [0x05, 0x00]);
        self.port.write_all(&[0x05, 0x00]).await?;

        let mut buf = [0; 2];
        self.port.read_exact(&mut buf).await?;
        trace!("<C {:x?}", buf);

        match buf {
            [0x63, 0x81] => Ok(true),
            [0x64, 0x80] => Ok(false),
            _ => Err(Error::new(
                ErrorKind::Other,
                format!("Unknown power_status: {:?}", buf),
            )),
        }
    }

    pub async fn power_on(&mut self) -> Result<(), Error> {
        self.set_power(true).await
    }

    pub async fn power_off(&mut self) -> Result<(), Error> {
        self.set_power(false).await
    }

    pub async fn set_power(&mut self, state: bool) -> Result<(), Error> {
        #[cfg(not(test))]
        self.port.clear(ClearBuffer::Input)?;
        let (cmd_byte, expected) = if state {
            (0x03, [0x63, 0x81])
        } else {
            (0x04, [0x64, 0x80])
        };
        trace!(">C {:x?}", [cmd_byte, 0x00]);
        self.port.write_all(&[cmd_byte, 0x00]).await?;

        let mut buf = [0; 2];
        self.port.read_exact(&mut buf).await?;
        trace!("<C {:x?}", buf);

        if buf == expected {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::Other,
                format!(
                    "Got an odd response after power-{}: {:?}",
                    if state { "on" } else { "off" },
                    buf
                ),
            ))
        }
    }

    pub async fn switch_to(&mut self, input: ComponentInput) -> Result<u8, Error> {
        if !self.power_status().await.unwrap_or_default() {
            self.power_on().await.ok();
            sleep(Duration::from_millis(500)).await;
        }

        let output: u8 = 1; // Output for component to TV - we're using output 1
        let input_byte: u8 = input.into();
        let selector = ((input_byte * 16) + 128) | output;

        trace!(">C {:x?}", [0x01, selector]);
        self.port.write_all(&[0x01, selector]).await?;

        let mut buf = [0; 2];
        self.port.read_exact(&mut buf).await?;
        trace!("<C {:x?}", buf);

        Ok((buf[1] & 0x30) >> 4)
    }
}

struct HDMISwitch {
    port: SerialStream,
    power_state: bool,
}

impl HDMISwitch {
    pub async fn switch_to(&mut self, input: HDMIInput) -> Result<(), Error> {
        if !self.power_state {
            self.power_on().await?;
            sleep(Duration::from_millis(500)).await;
        }
        let selector = Into::<u8>::into(input) + 0x30;
        trace!(">H {:x?}", b"port");
        trace!(">H {:x?}", &[selector, 0x52]);
        self.port.write_all(b"port").await?;
        self.port.write_all(&[selector, 0x52]).await
    }

    pub async fn power_on(&mut self) -> Result<(), Error> {
        trace!(">H {:x?}", b"poweronR");
        self.port.write_all(b"poweronR").await?;
        self.power_state = true;
        Ok(())
    }

    pub async fn power_off(&mut self) -> Result<(), Error> {
        trace!(">H {:x?}", b"poweroffR");
        self.port.write_all(b"poweroffR").await?;
        self.power_state = false;
        Ok(())
    }
}

pub struct Switcher {
    component: ComponentSwitch,
    hdmi: HDMISwitch,
    tv: TVSwitch,
    last_selected: String,
}

impl Switcher {
    pub fn new() -> Result<Switcher, Error> {
        Ok(Switcher {
            component: ComponentSwitch {
                port: open_serial(COMP_SWITCH_DEV)?,
            },
            hdmi: HDMISwitch {
                port: open_serial(HDMI_SWITCH_DEV)?,
                power_state: false,
            },
            tv: TVSwitch {
                port: open_serial(TV_SWITCH_DEV)?,
            },
            last_selected: "".to_string(),
        })
    }

    #[cfg(test)]
    pub fn test_new(component: SerialStream, hdmi: SerialStream, tv: SerialStream) -> Switcher {
        Switcher {
            component: ComponentSwitch { port: component },
            hdmi: HDMISwitch {
                port: hdmi,
                power_state: false,
            },
            tv: TVSwitch { port: tv },
            last_selected: "".to_string(),
        }
    }

    pub async fn tv_power_status(&mut self, any_response_ok: bool) -> Result<bool, Error> {
        self.tv.power_status(any_response_ok).await
    }

    pub async fn switch_to(&mut self, input_name: &str) -> Result<bool, Error> {
        // Don't switch to the same input as last switched to
        if self.last_selected.as_str() == input_name {
            return Ok(false);
        }

        if let Some(sw) = SWITCH_OPTIONS.get(input_name) {
            let tv_fut = self.tv.switch_to(sw.tv);
            let result = match sw.input {
                Some(CompOrHDMI::Component(comp_input)) => try_join![
                    self.component.switch_to(comp_input),
                    self.hdmi.power_off(),
                    tv_fut
                ]
                .map(|_| true),
                Some(CompOrHDMI::HDMI(hdmi_input)) => try_join![
                    self.component.power_off(),
                    self.hdmi.switch_to(hdmi_input),
                    tv_fut
                ]
                .map(|_| true),
                None => try_join![self.component.power_off(), self.hdmi.power_off(), tv_fut]
                    .map(|_| true),
            };
            if result.is_ok() {
                self.last_selected = input_name.to_string();
                if !cfg!(test) {
                    sleep(Duration::from_millis(1500)).await;
                }
                self.tv.set_aspect(sw.aspect).await?;
            }
            result
        } else {
            Err(Error::new(
                ErrorKind::Other,
                format!("Unknown input to switch to {}", input_name),
            ))
        }
    }
}

fn open_serial(path: &str) -> Result<tokio_serial::SerialStream, tokio_serial::Error> {
    tokio_serial::new(path, 9600)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .timeout(Duration::from_secs(1))
        .open_native_async()
}

pub fn get_switch_options() -> Vec<&'static str> {
    SWITCH_OPTIONS.keys().map(|s| *s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::TryFrom;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::runtime::Runtime;

    async fn test_power(response: [u8; 2]) -> Result<(bool, [u8; 2]), Error> {
        let (mut s1, s2) = SerialStream::pair().unwrap();
        let mut sw = ComponentSwitch { port: s2 };
        s1.write_all(&response).await?;

        let status = sw.power_status().await?;
        let mut buf = [0; 2];
        s1.read_exact(&mut buf).await?;

        Ok((status, buf))
    }

    #[test]
    fn test_power_off() {
        let rt = Runtime::new().unwrap();
        let f = test_power([0x63, 0x81]);
        assert_eq!(rt.block_on(f).unwrap(), (true, [5, 0]))
    }

    #[test]
    fn test_power_on() {
        let rt = Runtime::new().unwrap();
        let f = test_power([0x64, 0x80]);
        assert_eq!(rt.block_on(f).unwrap(), (false, [5, 0]))
    }

    #[test]
    fn test_power_unk() {
        let rt = Runtime::new().unwrap();
        let f = test_power([1, 1]);
        assert!(rt.block_on(f).is_err())
    }

    async fn do_cswitch(input: ComponentInput, response: [u8; 2]) -> Result<(u8, [u8; 2]), Error> {
        let (mut s1, s2) = SerialStream::pair().unwrap();
        let mut sw = ComponentSwitch { port: s2 };

        s1.write_all(&[0x63, 0x81]).await?;
        s1.write_all(&response).await?;

        let port = sw.switch_to(input).await?;

        let mut buf = [0; 2];
        s1.read_u16().await?;
        s1.read_exact(&mut buf).await?;

        Ok((port, buf))
    }

    #[test]
    fn test_cswitch() {
        let rt = Runtime::new().unwrap();
        let f1 = do_cswitch(ComponentInput::PS2, [0x61, 0x91]);
        assert_eq!(rt.block_on(f1).unwrap(), (1, [0x01, 0x91]));
        let f2 = do_cswitch(ComponentInput::Wii, [0x61, 0xa1]);
        assert_eq!(rt.block_on(f2).unwrap(), (2, [0x01, 0xa1]));
    }

    async fn do_hswitch(input: HDMIInput) -> Result<[u8; 6], Error> {
        let (mut s1, s2) = SerialStream::pair().unwrap();
        let mut sw = HDMISwitch {
            port: s2,
            power_state: false,
        };
        sw.switch_to(input).await?;
        let mut power_buf = [0; 8];
        s1.read_exact(&mut power_buf).await?;
        let mut buf = [0; 6];
        s1.read_exact(&mut buf).await?;
        Ok(buf)
    }

    #[test]
    fn test_hswitch() {
        let rt = Runtime::new().unwrap();
        for p in 0..5 {
            let f = do_hswitch(HDMIInput::try_from(p).unwrap());
            assert_eq!(rt.block_on(f).unwrap(), format!("port{}R", p).as_bytes());
        }
    }

    async fn do_tvswitch(input: TVInput, expected_size: usize) -> Result<Vec<u8>, Error> {
        let (mut s1, s2) = SerialStream::pair().unwrap();
        let mut sw = TVSwitch { port: s2 };
        for _ in 0..5 {
            s1.write_all(&[0x03, 0x0c, 0x1f]).await?;
        }
        sw.switch_to(input).await?;
        let mut output = Vec::with_capacity(expected_size);
        s1.take(expected_size as u64)
            .read_to_end(&mut output)
            .await?;
        Ok(output)
    }

    #[test]
    fn test_tvswitch() {
        env_logger::init();
        let rt = Runtime::new().unwrap();

        let f1 = do_tvswitch(TVInput::Component, 7);
        assert_eq!(
            rt.block_on(f1).unwrap(),
            vec![0x08, 0x22, 0x0a, 0x00, 0x03, 0x00, 0xc9]
        );

        let f2 = do_tvswitch(TVInput::HDMI1, 7);
        assert_eq!(
            rt.block_on(f2).unwrap(),
            vec![0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7]
        );

        let f3 = do_tvswitch(TVInput::HDMI2, 7);
        assert_eq!(
            rt.block_on(f3).unwrap(),
            vec![0x08, 0x22, 0x0a, 0x00, 0x05, 0x01, 0xc6]
        );

        let f4 = do_tvswitch(TVInput::SCART, 7);
        assert_eq!(
            rt.block_on(f4).unwrap(),
            vec![0x08, 0x22, 0x0d, 0x00, 0x00, 0x84, 0x45,]
        );
    }

    async fn do_switch(
        input: &str,
        comp_response: &[u8; 4],
        expected_responses: [Vec<u8>; 3],
    ) -> Result<(), Error> {
        let (mut c1, c2) = SerialStream::pair().unwrap();
        let (h1, h2) = SerialStream::pair().unwrap();
        let (mut t1, t2) = SerialStream::pair().unwrap();
        let mut switcher = Switcher::test_new(c2, h2, t2);
        c1.write_all(comp_response).await?;
        for _ in 0..5 {
            t1.write_all(&[0x03, 0x0c, 0x1f]).await?;
        }
        assert!(switcher.switch_to(input).await?);
        let mut comp_out = Vec::with_capacity(expected_responses[0].len());
        let mut hdmi_out = Vec::with_capacity(expected_responses[1].len());
        let mut tv_out = Vec::with_capacity(expected_responses[2].len());
        c1.take(expected_responses[0].len() as u64)
            .read_to_end(&mut comp_out)
            .await?;
        h1.take(expected_responses[1].len() as u64)
            .read_to_end(&mut hdmi_out)
            .await?;
        t1.take(expected_responses[2].len() as u64)
            .read_to_end(&mut tv_out)
            .await?;
        assert_eq!(comp_out, expected_responses[0]);
        assert_eq!(hdmi_out, expected_responses[1]);
        assert_eq!(tv_out, expected_responses[2]);
        Ok(())
    }

    #[test]
    fn test_switcher() {
        let rt = Runtime::new().unwrap();

        rt.block_on(do_switch(
            "PS2",
            &[0x63, 0x81, 0x61, 0x91],
            [
                vec![0x05, 0x00, 0x01, 0x91],
                "poweroffR".into(),
                vec![
                    0x08, 0x22, 0x0a, 0x00, 0x03, 0x00, 0xc9, 0x08, 0x22, 0x0b, 0x0a, 0x01, 0x04,
                    0xbc,
                ],
            ],
        ))
        .unwrap();

        rt.block_on(do_switch(
            "Wii",
            &[0x63, 0x81, 0x61, 0xa1],
            [
                vec![0x05, 0x00, 0x01, 0xa1],
                "poweroffR".into(),
                vec![0x08, 0x22, 0x0a, 0x00, 0x03, 0x00, 0xc9],
            ],
        ))
        .unwrap();

        rt.block_on(do_switch(
            "External",
            &[0x64, 0x80, 0, 0],
            [
                vec![0x04, 0x00],
                "poweronRport0R".into(),
                vec![0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7],
            ],
        ))
        .unwrap();

        rt.block_on(do_switch(
            "Switch",
            &[0x64, 0x80, 0, 0],
            [
                vec![0x04, 0x00],
                "poweronRport1R".into(),
                vec![
                    0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7, 0x08, 0x22, 0x0b, 0x0a, 0x01, 0x00,
                    0xc0,
                ],
            ],
        ))
        .unwrap();

        rt.block_on(do_switch(
            "PS4",
            &[0x64, 0x80, 0, 0],
            [
                vec![0x04, 0x00],
                "poweronRport2R".into(),
                vec![
                    0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7, 0x08, 0x22, 0x0b, 0x0a, 0x01, 0x00,
                    0xc0,
                ],
            ],
        ))
        .unwrap();

        rt.block_on(do_switch(
            "Fire TV Stick",
            &[0x64, 0x80, 0, 0],
            [
                vec![0x04, 0x00],
                "poweronRport3R".into(),
                vec![
                    0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7, 0x08, 0x22, 0x0b, 0x0a, 0x01, 0x00,
                    0xc0,
                ],
            ],
        ))
        .unwrap();

        rt.block_on(do_switch(
            "Dreamcast",
            &[0x64, 0x80, 0, 0],
            [
                vec![0x04, 0x00],
                "poweronRport4R".into(),
                vec![
                    0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7, 0x08, 0x22, 0x0b, 0x0a, 0x01, 0x04,
                    0xbc,
                ],
            ],
        ))
        .unwrap();

        rt.block_on(do_switch(
            "Kodi",
            &[0x64, 0x80, 0, 0],
            [
                vec![0x04, 0x00],
                "poweroffR".into(),
                vec![
                    0x08, 0x22, 0x0a, 0x00, 0x05, 0x01, 0xc6, 0x08, 0x22, 0x0b, 0x0a, 0x01, 0x00,
                    0xc0,
                ],
            ],
        ))
        .unwrap();

        rt.block_on(do_switch(
            "SCART",
            &[0x64, 0x80, 0, 0],
            [
                vec![0x04, 0x00],
                "poweroffR".into(),
                vec![
                    0x08, 0x22, 0x0d, 0x00, 0x00, 0x84, 0x45, 0x08, 0x22, 0x0b, 0x0a, 0x01, 0x04,
                    0xbc,
                ],
            ],
        ))
        .unwrap();
    }
}
