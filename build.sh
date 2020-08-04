#!/bin/sh
echo "Compiling for armv7-unknown-linux-gnueabihf..."
PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_PATH=/usr/lib/arm-linux-gnueabihf/pkgconfig cross build --features log_to_syslog --release --target armv7-unknown-linux-gnueabihf
echo "Compiling for aarch64-unknown-linux-gnu..."
PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig cross build --features log_to_syslog --release --target aarch64-unknown-linux-gnu
echo "All architectures done."
