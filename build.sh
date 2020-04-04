#!/bin/sh
PKG_CONFIG_ALLOW_CROSS=1 cross build --features log_to_syslog --release --target armv7-unknown-linux-musleabihf
