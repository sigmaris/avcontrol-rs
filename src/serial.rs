use log::trace;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use phf::phf_map;

use std::io::ErrorKind;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::try_join;
use tokio_serial::{
    DataBits, FlowControl, Parity, Serial, SerialPortSettings, StopBits,
};

const COMP_SWITCH_DEV: &str = "/dev/prolific0";
const HDMI_SWITCH_DEV: &str = "/dev/prolific1";
const TV_SWITCH_DEV: &str = "/dev/tvserial";
static SERIAL_SETTINGS: SerialPortSettings = SerialPortSettings {
    baud_rate: 9600,
    data_bits: DataBits::Eight,
    flow_control: FlowControl::None,
    parity: Parity::None,
    stop_bits: StopBits::One,
    timeout: Duration::from_secs(1),
};
static SWITCH_OPTIONS: phf::Map<&'static str, SwitchOption> = phf_map! {
    "PS2" =>           SwitchOption { input: Some(CompOrHDMI::Component(ComponentInput::PS2)), tv: TVInput::Component },
    "Wii" =>           SwitchOption { input: Some(CompOrHDMI::Component(ComponentInput::Wii)), tv: TVInput::Component },
    "External" =>      SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::External)),      tv: TVInput::HDMI1 },
    "Switch" =>        SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::Switch)),        tv: TVInput::HDMI1 },
    "PS4" =>           SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::PS4)),           tv: TVInput::HDMI1 },
    "Fire TV Stick" => SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::FireTVStick)),   tv: TVInput::HDMI1 },
    "Dreamcast" =>     SwitchOption { input: Some(CompOrHDMI::HDMI(HDMIInput::Dreamcast)),     tv: TVInput::HDMI1 },
    "Kodi" =>          SwitchOption { input: None, tv: TVInput::HDMI2 },
    "SCART" =>         SwitchOption { input: None, tv: TVInput::SCART },
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

pub struct SwitchOption {
    pub input: Option<CompOrHDMI>,
    pub tv: TVInput,
}

struct TVSwitch {
    port: Serial,
}

impl TVSwitch {
    pub async fn switch_to(&mut self, input: TVInput) -> Result<(), std::io::Error> {
        let codes: Vec<[u8; 4]> = match input {
            TVInput::SCART => {
                // Scart is a special case - there's no normal serial code for it, so do this:
                vec![
                    [0x0a, 0x00, 0x05, 0x01], // switch to HDMI2
                    [0x0d, 0x00, 0x00, 0x01], // press Source key
                    [0x0d, 0x00, 0x00, 0x61], // press Down key
                    [0x0d, 0x00, 0x00, 0x68], // press Enter key
                ]
            }
            other => {
                let other_bytes: u16 = other.into();
                // Bytes 1&2 are always 0x0a 0x00, byte 3 is source and 4 is index (e.g. 0/1 for HDMI1/2)
                vec![[
                    0x0a,
                    0x00,
                    ((other_bytes & 0xff00) >> 8) as u8,
                    (other_bytes & 0xff) as u8,
                ]]
            }
        };
        for [a, b, c, d] in codes {
            let prefixed_cmd = [0x08, 0x22, a, b, c, d];
            let sum: u32 = prefixed_cmd.iter().map(|x| *x as u32).sum();
            let checksum: u8 = ((256 - sum) % 256) as u8;

            trace!(">T {:x?}", prefixed_cmd);
            trace!(">T {:x?}", [checksum]);

            self.port.write_all(&prefixed_cmd).await?;
            self.port.write_u8(checksum).await?;
        }
        Ok(())
    }
}

struct ComponentSwitch {
    port: Serial,
}

impl ComponentSwitch {
    // Is power on, off, or unknown (error)?
    pub async fn power_status(&mut self) -> Result<bool, std::io::Error> {
        trace!(">C {:x?}", [0x05, 0x00]);
        self.port.write_all(&[0x05, 0x00]).await?;

        let mut buf = [0; 2];
        self.port.read_exact(&mut buf).await?;
        trace!("<C {:x?}", buf);

        match buf {
            [0x63, 0x81] => Ok(true),
            [0x64, 0x80] => Ok(false),
            _ => Err(std::io::Error::new(
                ErrorKind::Other,
                format!("Unknown power_status: {:?}", buf),
            )),
        }
    }

    pub async fn power_on(&mut self) -> Result<(), std::io::Error> {
        trace!(">C {:x?}", [0x03, 0x00]);
        self.port.write_all(&[0x03, 0x00]).await?;

        let mut buf = [0; 2];
        self.port.read_exact(&mut buf).await?;
        trace!("<C {:x?}", buf);

        match buf {
            [0x63, 0x81] => Ok(()),
            _ => Err(std::io::Error::new(
                ErrorKind::Other,
                format!("Got an odd response after power-on: {:?}", buf),
            )),
        }
    }

    pub async fn switch_to(&mut self, input: &ComponentInput) -> Result<u8, std::io::Error> {
        if !self.power_status().await.unwrap_or_default() {
            self.power_on().await.ok();
        }

        let output: u8 = 1; // Output for component to TV - we're using output 1
        let input_byte: u8 = (*input).into();
        let selector = ((input_byte * 16) + 128) | output;

        trace!(">C {:x?}", [0x01, selector]);
        self.port.write_all(&[0x01, selector]).await?;

        let mut buf = [0; 2];
        self.port.read_exact(&mut buf).await?;
        trace!("<C {:x?}", buf);

        Ok((buf[1] & 0x30) >> 4)
    }
}

// "port" in ASCII
const HDMI_PORT_STR: [u8; 4] = [0x70, 0x6f, 0x72, 0x74];

struct HDMISwitch {
    port: Serial,
}

impl HDMISwitch {
    pub async fn switch_to(&mut self, input: &HDMIInput) -> Result<(), std::io::Error> {
        let selector = Into::<u8>::into(*input) + 0x30;
        trace!(">H {:x?}", &HDMI_PORT_STR);
        trace!(">H {:x?}", &[selector, 0x52]);
        self.port.write_all(&HDMI_PORT_STR).await?;
        self.port.write_all(&[selector, 0x52]).await
    }
}

pub struct Switcher {
    component: ComponentSwitch,
    hdmi: HDMISwitch,
    tv: TVSwitch,
}

impl Switcher {
    pub fn new() -> Result<Switcher, std::io::Error> {
        Ok(Switcher {
            component: ComponentSwitch {
                port: Serial::from_path(COMP_SWITCH_DEV, &SERIAL_SETTINGS)?,
            },
            hdmi: HDMISwitch {
                port: Serial::from_path(HDMI_SWITCH_DEV, &SERIAL_SETTINGS)?,
            },
            tv: TVSwitch {
                port: Serial::from_path(TV_SWITCH_DEV, &SERIAL_SETTINGS)?,
            },
        })
    }

    #[cfg(test)]
    pub fn test_new(component: Serial, hdmi: Serial, tv: Serial) -> Switcher {
        Switcher {
            component: ComponentSwitch {
                port: component,
            },
            hdmi: HDMISwitch {
                port: hdmi,
            },
            tv: TVSwitch {
                port: tv,
            },
        }
    }

    pub async fn switch_to(&mut self, input_name: &str) -> Result<(), std::io::Error> {
        if let Some(sw) = SWITCH_OPTIONS.get(input_name) {
            let tv_future = self.tv.switch_to(sw.tv);
            match sw.input {
                Some(CompOrHDMI::Component(comp_input)) =>
                    try_join![self.component.switch_to(&comp_input), tv_future].map(|_| ()),
                Some(CompOrHDMI::HDMI(hdmi_input)) => try_join![self.hdmi.switch_to(&hdmi_input), tv_future].map(|_| ()),
                None => tv_future.await
            }
        } else {
            Err(std::io::Error::new(
                ErrorKind::Other,
                format!("Unknown input to switch to {}", input_name),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::TryFrom;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::runtime::Runtime;

    async fn test_power(response: [u8; 2]) -> Result<(bool, [u8; 2]), std::io::Error> {
        let (mut s1, s2) = Serial::pair().unwrap();
        let mut sw = ComponentSwitch { port: s2 };
        s1.write_all(&response).await?;

        let status = sw.power_status().await?;
        let mut buf = [0; 2];
        s1.read_exact(&mut buf).await?;

        Ok((status, buf))
    }

    #[test]
    fn test_power_off() {
        let mut rt = Runtime::new().unwrap();
        let f = test_power([0x63, 0x81]);
        assert_eq!(rt.block_on(f).unwrap(), (true, [5, 0]))
    }

    #[test]
    fn test_power_on() {
        let mut rt = Runtime::new().unwrap();
        let f = test_power([0x64, 0x80]);
        assert_eq!(rt.block_on(f).unwrap(), (false, [5, 0]))
    }

    #[test]
    fn test_power_unk() {
        let mut rt = Runtime::new().unwrap();
        let f = test_power([1, 1]);
        assert!(rt.block_on(f).is_err())
    }

    async fn do_cswitch(
        input: ComponentInput,
        response: [u8; 2],
    ) -> Result<(u8, [u8; 2]), std::io::Error> {
        let (mut s1, s2) = Serial::pair().unwrap();
        let mut sw = ComponentSwitch { port: s2 };

        s1.write_all(&[0x63, 0x81]).await?;
        s1.write_all(&response).await?;

        let port = sw.switch_to(&input).await?;

        let mut buf = [0; 2];
        s1.read_u16().await?;
        s1.read_exact(&mut buf).await?;

        Ok((port, buf))
    }

    #[test]
    fn test_cswitch() {
        let mut rt = Runtime::new().unwrap();
        let f1 = do_cswitch(ComponentInput::PS2, [0x61, 0x91]);
        assert_eq!(rt.block_on(f1).unwrap(), (1, [0x01, 0x91]));
        let f2 = do_cswitch(ComponentInput::Wii, [0x61, 0xa1]);
        assert_eq!(rt.block_on(f2).unwrap(), (2, [0x01, 0xa1]));
    }

    async fn do_hswitch(input: HDMIInput) -> Result<[u8; 6], std::io::Error> {
        let (mut s1, s2) = Serial::pair().unwrap();
        let mut sw = HDMISwitch { port: s2 };
        sw.switch_to(&input).await?;
        let mut buf = [0; 6];
        s1.read_exact(&mut buf).await?;
        Ok(buf)
    }

    #[test]
    fn test_hswitch() {
        let mut rt = Runtime::new().unwrap();
        for p in 0..5 {
            let f = do_hswitch(HDMIInput::try_from(p).unwrap());
            assert_eq!(rt.block_on(f).unwrap(), format!("port{}R", p).as_bytes());
        }
    }

    async fn do_tvswitch(input: TVInput, expected_size: usize) -> Result<Vec<u8>, std::io::Error> {
        let (s1, s2) = Serial::pair().unwrap();
        let mut sw = TVSwitch { port: s2 };
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
        let mut rt = Runtime::new().unwrap();

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

        let f4 = do_tvswitch(TVInput::SCART, 28);
        assert_eq!(
            rt.block_on(f4).unwrap(),
            vec![
                0x08, 0x22, 0x0a, 0x00, 0x05, 0x01, 0xc6,
                0x08, 0x22, 0x0d, 0x00, 0x00, 0x01, 0xc8,
                0x08, 0x22, 0x0d, 0x00, 0x00, 0x61, 0x68,
                0x08, 0x22, 0x0d, 0x00, 0x00, 0x68, 0x61,
            ]
        );
    }

    async fn do_switch(input: &str, comp_response: &[u8; 2], expected_responses: [Vec<u8>; 3]) -> Result<(), std::io::Error> {
        let (mut c1, c2) = Serial::pair().unwrap();
        let (h1, h2) = Serial::pair().unwrap();
        let (t1, t2) = Serial::pair().unwrap();
        let mut switcher = Switcher::test_new(c2, h2, t2);
        c1.write_all(&[0x63, 0x81]).await?;
        c1.write_all(comp_response).await?;
        switcher.switch_to(input).await?;
        let mut comp_out = Vec::with_capacity(expected_responses[0].len());
        let mut hdmi_out = Vec::with_capacity(expected_responses[1].len());
        let mut tv_out = Vec::with_capacity(expected_responses[2].len());
        c1.take(expected_responses[0].len() as u64).read_to_end(&mut comp_out).await?;
        h1.take(expected_responses[1].len() as u64).read_to_end(&mut hdmi_out).await?;
        t1.take(expected_responses[2].len() as u64).read_to_end(&mut tv_out).await?;
        assert_eq!(comp_out, expected_responses[0]);
        assert_eq!(hdmi_out, expected_responses[1]);
        assert_eq!(tv_out, expected_responses[2]);
        Ok(())
    }

    #[test]
    fn test_switcher() {
        let mut rt = Runtime::new().unwrap();

        rt.block_on(do_switch("PS2", &[0x61, 0x91], [
            vec![0x05, 0x00, 0x01, 0x91], vec![], vec![0x08, 0x22, 0x0a, 0x00, 0x03, 0x00, 0xc9]
        ])).unwrap();

        rt.block_on(do_switch("Wii", &[0x61, 0xa1], [
            vec![0x05, 0x00, 0x01, 0xa1], vec![], vec![0x08, 0x22, 0x0a, 0x00, 0x03, 0x00, 0xc9]
        ])).unwrap();

        rt.block_on(do_switch("External", &[0, 0], [
            vec![], vec![0x70, 0x6f, 0x72, 0x74, 0x30, 0x52], vec![0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7]
        ])).unwrap();

        rt.block_on(do_switch("Switch", &[0, 0], [
            vec![], vec![0x70, 0x6f, 0x72, 0x74, 0x31, 0x52], vec![0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7]
        ])).unwrap();

        rt.block_on(do_switch("PS4", &[0, 0], [
            vec![], vec![0x70, 0x6f, 0x72, 0x74, 0x32, 0x52], vec![0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7]
        ])).unwrap();

        rt.block_on(do_switch("Fire TV Stick", &[0, 0], [
            vec![], vec![0x70, 0x6f, 0x72, 0x74, 0x33, 0x52], vec![0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7]
        ])).unwrap();

        rt.block_on(do_switch("Dreamcast", &[0, 0], [
            vec![], vec![0x70, 0x6f, 0x72, 0x74, 0x34, 0x52], vec![0x08, 0x22, 0x0a, 0x00, 0x05, 0x00, 0xc7]
        ])).unwrap();

        rt.block_on(do_switch("Kodi", &[0,0], [vec![], vec![], vec![
            0x08, 0x22, 0x0a, 0x00, 0x05, 0x01, 0xc6,
        ]])).unwrap();

        rt.block_on(do_switch("SCART", &[0,0], [vec![], vec![], vec![
            0x08, 0x22, 0x0a, 0x00, 0x05, 0x01, 0xc6,
            0x08, 0x22, 0x0d, 0x00, 0x00, 0x01, 0xc8,
            0x08, 0x22, 0x0d, 0x00, 0x00, 0x61, 0x68,
            0x08, 0x22, 0x0d, 0x00, 0x00, 0x68, 0x61,
        ]])).unwrap();
    }
}
