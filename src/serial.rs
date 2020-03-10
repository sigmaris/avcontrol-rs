use log::trace;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use std::convert::TryFrom;
use std::io::ErrorKind;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;
use tokio_serial::{ClearBuffer, DataBits, FlowControl, Parity, Serial, SerialPort, SerialPortSettings, StopBits};

static SERIAL_SETTINGS: SerialPortSettings = SerialPortSettings {
    baud_rate: 9600,
    data_bits: DataBits::Eight,
    flow_control: FlowControl::None,
    parity: Parity::None,
    stop_bits: StopBits::One,
    timeout: Duration::from_secs(1),
};

#[derive(IntoPrimitive, TryFromPrimitive)]
#[repr(u16)]
enum TVInput {
    Component = 0x300,
    HDMI1 = 0x500,
    HDMI2 = 0x501,
    SCART,
}

#[derive(IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum ComponentInput {
    PS2 = 1,
    Wii = 2,
}

#[derive(IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum HDMIInput {
    External = 0,
    Switch = 1,
    PS4 = 2,
    FireTVStick = 3,
    Dreamcast = 4,
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
                    [0x0a, 0x00, 0x05, 0x01],  // switch to HDMI2
                    [0x0d, 0x00, 0x00, 0x01],  // press Source key
                    [0x0d, 0x00, 0x00, 0x61],  // press Down key
                    [0x0d, 0x00, 0x00, 0x68],  // press Enter key
                ]
            }
            other => {
                let other_bytes: u16 = other.into();
                // Bytes 1&2 are always 0x0a 0x00, byte 3 is source and 4 is index (e.g. 0/1 for HDMI1/2)
                vec![[0x0a, 0x00, ((other_bytes & 0xff00) >> 8) as u8, (other_bytes & 0xff) as u8]]
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

    pub async fn switch_to(&mut self, input: ComponentInput) -> Result<u8, std::io::Error> {
        if !self.power_status().await.unwrap_or_default() {
            self.power_on().await.ok();
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

// "port" in ASCII
const HDMI_PORT_STR: [u8; 4] = [0x70, 0x6f, 0x72, 0x74];

struct HDMISwitch {
    port: Serial,
}

impl HDMISwitch {
    pub async fn switch_to(&mut self, input: HDMIInput) -> Result<(), std::io::Error> {
        let selector = Into::<u8>::into(input) + 0x30;
        trace!(">H {:x?}", &HDMI_PORT_STR);
        trace!(">H {:x?}", &[selector, 0x52]);
        self.port.write_all(&HDMI_PORT_STR).await?;
        self.port.write_all(&[selector, 0x52]).await
    }
}

pub fn get_serial_ports() -> Result<(Serial, Serial, Serial), std::io::Error> {
    Ok((
        Serial::from_path("/dev/prolific0", &SERIAL_SETTINGS)?,
        Serial::from_path("/dev/prolific1", &SERIAL_SETTINGS)?,
        Serial::from_path("/dev/tvserial", &SERIAL_SETTINGS)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    async fn do_cswitch(input: ComponentInput, response: [u8; 2]) -> Result<(u8, [u8; 2]), std::io::Error> {
        let (mut s1, s2) = Serial::pair().unwrap();
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
        let mut rt = Runtime::new().unwrap();
        let f1 = do_cswitch(ComponentInput::PS2, [0x61, 0x91]);
        assert_eq!(rt.block_on(f1).unwrap(), (1, [0x01, 0x91]));
        let f2 = do_cswitch(ComponentInput::Wii, [0x61, 0xa1]);
        assert_eq!(rt.block_on(f2).unwrap(), (2, [0x01, 0xa1]));
    }

    async fn do_hswitch(input: HDMIInput) -> Result<[u8; 6], std::io::Error> {
        let (mut s1, s2) = Serial::pair().unwrap();
        let mut sw = HDMISwitch { port: s2 };
        sw.switch_to(input).await?;
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
}
