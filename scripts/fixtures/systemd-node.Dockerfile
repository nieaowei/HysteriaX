ARG BASE_IMAGE=debian:trixie
FROM ${BASE_IMAGE}

ENV container=docker
STOPSIGNAL SIGRTMIN+3

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
       ca-certificates curl iptables nftables openssh-server python3 sudo systemd systemd-sysv \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /root/.ssh /etc/ssh/sshd_config.d \
    && printf 'PermitRootLogin prohibit-password\nPasswordAuthentication no\nPubkeyAuthentication yes\n' \
       > /etc/ssh/sshd_config.d/99-hysteriax-test.conf

# Keep a privileged systemd test node from unregistering the host's QEMU binfmt handlers.
RUN ln -sf /dev/null /etc/systemd/system/systemd-binfmt.service \
    && ln -sf /dev/null /etc/systemd/system/systemd-binfmt.socket

CMD ["/lib/systemd/systemd"]
