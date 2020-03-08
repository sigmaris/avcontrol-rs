use std::io::ErrorKind;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Runtime;
use tokio_serial::{DataBits, FlowControl, Parity, Serial, SerialPortSettings, StopBits};

static SERIAL_SETTINGS: SerialPortSettings = SerialPortSettings {
    baud_rate: 9600,
    data_bits: DataBits::Eight,
    flow_control: FlowControl::None,
    parity: Parity::None,
    stop_bits: StopBits::One,
    timeout: Duration::from_secs(1),
};

struct ComponentSwitch {
    port: Serial,
}

impl ComponentSwitch {
    pub async fn power_status(&mut self) -> Result<bool, std::io::Error> {
        self.port.write_all(&[5, 0]).await?;
        let mut buf = [0; 2];
        self.port.read_exact(&mut buf).await?;
        match buf {
            [0x63, 0x81] => Ok(true),
            [0x64, 0x80] => Ok(false),
            _ => Err(std::io::Error::new(
                ErrorKind::Other,
                format!("Unknown power_status: {:?}", buf),
            )),
        }
    }
}

/*
pub fn get_serial_ports() -> Result<(Serial, Serial, Serial), std::io::Error> {
    Ok((
        Serial::from_path("/dev/prolific0", &SERIAL_SETTINGS)?,
        Serial::from_path("/dev/prolific1", &SERIAL_SETTINGS)?,
        Serial::from_path("/dev/tvserial", &SERIAL_SETTINGS)?,
    ))
}
*/

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn test_power(response: [u8; 2]) -> Result<(bool, [u8; 2]), std::io::Error> {
        let (mut s1, mut s2) = Serial::pair().unwrap();
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
}
