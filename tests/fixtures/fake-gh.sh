#!/bin/sh
printf '%s\n' "$*" >> "${0}.log"
case "$0" in
  *-existing-*) mode=existing ;;
  *-new-*) mode=new ;;
  *-invalid-*) mode=invalid ;;
  *-nonzero-*) mode=nonzero ;;
  *) mode=unknown ;;
esac
if [ "$2" = list ]; then
  case "$mode" in
    existing) printf '%s' '[{"url":"https://github/pr/99"}]' ;;
    invalid) printf '%s' '{' ;;
    nonzero) printf '%s' 'list failed' >&2; exit 17 ;;
    *) printf '%s' '[]' ;;
  esac
else
  touch "${0}.created"
  printf '%s' 'https://github/pr/new'
fi
