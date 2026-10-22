#!/usr/bin/env bash
# Structural check for the deck. Run after any edit to index.html.
#
# Catches the failure that actually bit us: an edit that splices slides in or
# out can leave stale <section> blocks OUTSIDE the .slides container. Reveal
# ignores them, so the deck still presents and screenshots still look right —
# but the orphans render as stray text on screen and spill the PDF export onto
# extra garbled pages. Balanced counts are the cheap way to see it.
set -euo pipefail
cd "$(dirname "$0")"

sections=$(grep -c '<section' index.html)
asides=$(grep -c '<aside class="notes">' index.html)
open_s=$(grep -o '<section' index.html | wc -l)
close_s=$(grep -o '</section>' index.html | wc -l)
closers=$(grep -c '^</div></div>$' index.html)

fail=0
[ "$open_s" -eq "$close_s" ] || { echo "FAIL: $open_s <section> vs $close_s </section>"; fail=1; }
[ "$sections" -eq "$asides" ] || { echo "FAIL: $sections sections but $asides notes blocks — every slide needs exactly one"; fail=1; }
[ "$closers" -eq 1 ] || { echo "FAIL: $closers '</div></div>' closers at line start — expected 1; orphaned slides outside .slides?"; fail=1; }

for pdf in deck.pdf speaker-notes.pdf; do
  if [ -f "$pdf" ] && [ "index.html" -nt "$pdf" ]; then
    echo "STALE: $pdf is older than index.html — regenerate it before committing"
    echo "       deck.pdf:          open index.html?print-pdf and print to PDF"
    echo "       speaker-notes.pdf: node build-notes.js"
    fail=1
  fi
done

if [ "$fail" -eq 0 ]; then
  echo "OK — $sections slides, $asides notes blocks, markup balanced, PDFs current."
else
  echo
  echo "The headless render check will NOT catch this: reveal drops orphaned"
  echo "sections from its slide list, so screenshots stay clean while the PDF"
  echo "export and the live screen pick up the stray markup."
  exit 1
fi
