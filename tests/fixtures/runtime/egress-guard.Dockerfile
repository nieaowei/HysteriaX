FROM alpine:3.22
RUN apk add --no-cache iptables
COPY tests/fixtures/runtime/egress-guard.sh /usr/local/bin/hysteriax-fixture-egress-guard
ENTRYPOINT ["/bin/sh", "/usr/local/bin/hysteriax-fixture-egress-guard"]
