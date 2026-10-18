#!/bin/sh
case "$1" in
  "Username for 'https://github.com':"*) printf '%s\n' 'x-access-token' ;;
  "Password for 'https://x-access-token@github.com':"*) printf '%s\n' "$PUBLISH_GITHUB_TOKEN" ;;
  *) exit 1 ;;
esac
