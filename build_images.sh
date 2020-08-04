#!/bin/sh -e
docker build -t sigmaris/avcontrolbuilder:armv7-unknown-linux-gnueabihf --build-arg TARGET=armv7-unknown-linux-gnueabihf --build-arg DEB_ARCH=armhf .
docker build -t sigmaris/avcontrolbuilder:aarch64-unknown-linux-gnu --build-arg TARGET=aarch64-unknown-linux-gnu --build-arg DEB_ARCH=arm64 .

