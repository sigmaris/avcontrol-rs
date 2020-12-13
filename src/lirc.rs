use std::os::unix::io::AsRawFd; 

use log::{debug, warn};
use nix::{ioctl_read, ioctl_write_ptr};
use tokio::io::AsyncWriteExt;
use tokio::fs::{OpenOptions, File};

const KEY_POWER: u32 = 0b11111101000000100000011100000111;

const LIRC_IOC_MAGIC: u8 = b'i';

const LIRC_GET_FEATURES: u32 = 0;
const LIRC_SET_SEND_MODE: u32 = 0x00000011;

const LIRC_MODE_PULSE: u32 = 0x00000002;

ioctl_read!(lirc_get_features, LIRC_IOC_MAGIC, LIRC_GET_FEATURES, u32);
ioctl_write_ptr!(lirc_set_send_mode, LIRC_IOC_MAGIC, LIRC_SET_SEND_MODE, u32);

pub struct LircSender {
    device: File,
}

impl LircSender {
    pub async fn find_device() -> Option<LircSender> {
        let mut index = 0;
        loop {
            let devpath = format!("/dev/lirc{}", index);
            match OpenOptions::new().write(true).open(&devpath).await {
                Ok(device) => {
                    let mut features: u32 = 0;
                    let fd = device.as_raw_fd();
                    if let Ok(_) = unsafe {
                        lirc_get_features(fd, &mut features)
                    } {
                         if (features & LIRC_MODE_PULSE) == LIRC_MODE_PULSE {
                            if let Ok(_) = unsafe {
                                lirc_set_send_mode(fd, &LIRC_MODE_PULSE)
                            } {
                                debug!("Using {} for LIRC", &devpath);
                                return Some(LircSender { device });
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Can't open {}: {}", &devpath, e);
                    return None;
                }
            }
            index += 1;
        }
    }

    pub async fn send_power_key(&mut self) -> Result<(), std::io::Error> {
        for _times in 0..1 {
            let mut buf: Vec<u8> = Vec::with_capacity(268);
            buf.extend_from_slice(&4500_i32.to_ne_bytes());
            buf.extend_from_slice(&4500_i32.to_ne_bytes());
            for i in 0..32 {
                if (KEY_POWER & (1u32 << i)) == (1u32 << i) {
                    buf.extend_from_slice(&560_i32.to_ne_bytes());
                    buf.extend_from_slice(&1690_i32.to_ne_bytes());
                } else {
                    buf.extend_from_slice(&560_i32.to_ne_bytes());
                    buf.extend_from_slice(&560_i32.to_ne_bytes());
                }
            }
            buf.extend_from_slice(&560_i32.to_ne_bytes());
            debug!("Wrote {} bytes to LIRC device", self.device.write(&buf).await?);
        }
        Ok(())
    }
}
