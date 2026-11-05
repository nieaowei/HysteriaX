#!/bin/sh
# Test-only helper. Run in a newly owned fixture container's network namespace,
# never the host namespace. The target must start with IPv6 disabled and keep
# its application behind a startup gate until these rules and probes pass.
set -eu
[ "$#" -eq 2 ] || exit 64
[ "$(cat /proc/sys/net/ipv6/conf/all/disable_ipv6)" = 1 ] || exit 65
postgres_ip=$1
redis_ip=$2
iptables -w -P OUTPUT DROP
iptables -w -A OUTPUT -o lo -j ACCEPT
iptables -w -A OUTPUT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -w -A OUTPUT -d "$postgres_ip" -p tcp --dport 5432 -j ACCEPT
iptables -w -A OUTPUT -d "$redis_ip" -p tcp --dport 6379 -j ACCEPT
iptables -w -S OUTPUT
