#!/bin/sh
# Writes the resolver nginx.conf includes to /tmp, since the root is read-only. The
# repository map comes from agent-run (/etc/nginx/agent-run/github-repos.conf).
awk '/^nameserver/ { s = s " " $2 } END { print "resolver" s ";" }' /etc/resolv.conf >/tmp/resolver.conf
exec nginx -g 'daemon off;'
