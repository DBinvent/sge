#!/bin/bash
set -Eeu
# The man pages, one for each language `sge --help` speaks (src/i18n/<lang>_clc.yaml):
# help2man over that language's --help, with the NAME line and the EXAMPLES section of
# man/<lang>.h2m. Written as a man tree, laid out as /usr/share/man is:
#
#   <out>/man1/sge.1.gz          English
#   <out>/<dir>/man1/sge.1.gz    the others, in the directory man-db reads for the
#                                language: ru, fr, de, ja, and zh_CN for zh - which is
#                                Simplified Chinese
#
#   man/build.sh <sge binary> <out dir>
#
# help2man runs the program with LANG removed and LC_ALL set to its -L locale. C.UTF-8:
# help2man reads the output in that locale's encoding, and plain C would read Cyrillic
# and CJK as ASCII. sge picks its language by LANG, so it is run through a wrapper that
# sets it. help2man's own words come out in English that way - no locale but C.UTF-8
# need be installed - and are put into the language here, as help2man's own translations
# have them: the section headings, and the manual's name in the page header (by sed:
# help2man would encode a --manual argument as UTF-8 a second time).

SGE=$(realpath "${1:?usage: man/build.sh <sge binary> <out dir>}")
OUT=${2:?usage: man/build.sh <sge binary> <out dir>}
MAN_DIR=$(dirname "$(realpath "$0")")
command -v help2man >/dev/null || { echo "ERROR: help2man is not installed: sudo apt install help2man" >&2; exit 1; }

# the page date: SOURCE_DATE_EPOCH for a reproducible build, today otherwise
DATE=$(date -u -d "@${SOURCE_DATE_EPOCH:-$(date +%s)}" +%Y-%m-%d)
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT

# lang  dir    NAME          SYNOPSIS   DESCRIPTION   OPTIONS     EXAMPLES   Commands:      manual
while IFS='|' read -r lang dir name synopsis description options examples commands manual; do
  [ -f "$MAN_DIR/$lang.h2m" ] || { echo "ERROR: no $MAN_DIR/$lang.h2m" >&2; exit 1; }
  mkdir -p "$TMP/$lang" "$OUT/$dir/man1"
  printf '#!/bin/sh\nLANG=%s exec "%s" "$@"\n' "$lang" "$SGE" > "$TMP/$lang/sge"
  chmod +x "$TMP/$lang/sge"
  page="$OUT/$dir/man1/sge.1"
  # -N: no pointer to a Texinfo manual, which sge does not have
  help2man -N -L C.UTF-8 -i "$MAN_DIR/$lang.h2m" -o "$page" "$TMP/$lang/sge"
  # an ISO date and the manual's name in the language; each " [default: ...]" clap
  # adds on a line of its own; the headings in the language
  sed -i -E \
    -e "2s/^(\\.TH SGE \"1\" )\"[^\"]*\"/\\1\"$DATE\"/" \
    -e "2s/\"User Commands\"\$/\"$manual\"/" \
    -e 's/ \[default: /\n [default: /g' \
    -e "s/^\\.SH NAME\$/.SH $name/" \
    -e "s/^\\.SH SYNOPSIS\$/.SH $synopsis/" \
    -e "s/^\\.SH DESCRIPTION\$/.SH $description/" \
    -e "s/^\\.SH OPTIONS\$/.SH $options/" \
    -e "s/^\\.SH EXAMPLES\$/.SH $examples/" \
    -e "s/^\\.SS \"Commands:\"\$/.SS \"$commands\"/" \
    "$page"
  gzip -f -9 -n "$page"
  echo "  $page.gz"
done <<'EOF'
en|.|NAME|SYNOPSIS|DESCRIPTION|OPTIONS|EXAMPLES|Commands:|User Commands
ru|ru|ИМЯ|ОБЗОР|ОПИСАНИЕ|ПАРАМЕТРЫ|ПРИМЕРЫ|Команды:|Пользовательские команды
fr|fr|NOM|SYNOPSIS|DESCRIPTION|OPTIONS|EXEMPLES|Commandes :|Commandes
de|de|BEZEICHNUNG|ÜBERSICHT|BESCHREIBUNG|OPTIONEN|BEISPIELE|Befehle:|Dienstprogramme für Benutzer
zh|zh_CN|名称|摘要|描述|选项|示例|命令：|用户命令
ja|ja|名前|書式|説明|オプション|例|コマンド：|ユーザーコマンド
EOF
