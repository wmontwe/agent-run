#!/bin/sh
# Writes what nginx.conf includes to /tmp, since the root is read-only. Lines
# that aren't plain owner/repo match nothing here; bin/agent refuses them.
awk '/^nameserver/ { s = s " " $2 } END { print "resolver" s ";" }' /etc/resolv.conf >/tmp/resolver.conf
awk '/^[A-Za-z0-9_-][A-Za-z0-9._-]*\/[A-Za-z0-9._-]+$/ && !/\.\./ {
    gsub(/\./, "\\.")
    print "~*^/repos/" $0 "(/|\\?|$) 1;"
}' /etc/github-repos.txt >/tmp/github-repos.conf
exec nginx -g 'daemon off;'
