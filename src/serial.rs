use log::trace;
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

// "port" in ASCII
const HDMI_PORT_STR: [u8; 4] = [0x70, 0x6f, 0x72, 0x74];

struct ComponentSwitch {
    port: Serial,
}

impl ComponentSwitch {
    // Is power on, off, or unknown (error)
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

    pub async fn switch_to(&mut self, input: u8) -> Result<u8, std::io::Error> {
        if !self.power_status().await.unwrap_or_default() {
            self.power_on().await.ok();
        }
        let output = 1; // Output for component to TV
        let selector = ((input * 16) + 128) | output;
        trace!(">C {:x?}", [0x01, selector]);
        self.port.write_all(&[0x01, selector]).await?;
        let mut buf = [0; 2];
        self.port.read_exact(&mut buf).await?;
        trace!("<C {:x?}", buf);
        Ok((buf[1] & 0x30) >> 4)
    }
}

struct HDMISwitch {
    port: Serial,
}

impl HDMISwitch {
    pub async fn switch_to(&mut self, input: u8) -> Result<(), std::io::Error> {
        let selector = 0x30 + input;
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

    async fn do_cswitch(input: u8, response: [u8; 2]) -> Result<(u8, [u8; 2]), std::io::Error> {
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
        let f1 = do_cswitch(1, [0x61, 0x91]);
        assert_eq!(rt.block_on(f1).unwrap(), (1, [0x01, 0x91]));
        let f2 = do_cswitch(2, [0x61, 0xa1]);
        assert_eq!(rt.block_on(f2).unwrap(), (2, [0x01, 0xa1]));
    }

    async fn do_hswitch(input: u8) -> Result<[u8; 6], std::io::Error> {
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
            let f = do_hswitch(p);
            assert_eq!(rt.block_on(f).unwrap(), format!("port{}R", p).as_bytes());
        }
    }
}
