ARG TARGET

FROM rustembedded/cross:$TARGET-0.2.1

ARG DEB_ARCH

RUN dpkg --add-architecture $DEB_ARCH && \
    apt-get update && \
    apt-get install --assume-yes libudev1:$DEB_ARCH libudev-dev:$DEB_ARCH
