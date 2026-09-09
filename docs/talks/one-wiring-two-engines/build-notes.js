const { chromium } = require('/tmp/pw/node_modules/playwright');
const fs = require('fs');
(async () => {
  const b = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium-1194/chrome-linux/chrome' });
  const p = await b.newPage({ viewport:{width:1280,height:800} });
  await p.goto('file:///home/user/wingfoil/docs/talks/one-wiring-two-engines/index.html',
               { waitUntil:'networkidle' });
  await p.waitForTimeout(800);
  const n = await p.evaluate(() => Reveal.getTotalSlides());
  const rows = [];
  for (let i = 0; i < n; i++) {
    await p.evaluate(k => Reveal.slide(k), i);
    await p.waitForTimeout(260);
    const file = `/tmp/pw/nt_${String(i+1).padStart(2,'0')}.jpg`;
    await p.screenshot({ path: file, type:'jpeg', quality: 72 });
    const d = await p.evaluate(() => {
      const s = Reveal.getCurrentSlide();
      const h = s.querySelector('h1,h2'), k = s.querySelector('.kicker'),
            a = s.querySelector('aside.notes');
      return { title:(h?h.textContent:'').trim(), kicker:(k?k.textContent:'').trim(),
               notes: a ? a.innerHTML : '' };
    });
    rows.push({ i:i+1, file: file.split('/').pop(), ...d });
  }
  await b.close();

  fs.writeFileSync('/tmp/pw/notes.html', `<!doctype html><meta charset="utf-8">
<title>wingfoil — speaker notes</title><style>
@page { size: A4 portrait; margin: 13mm 11mm; }
body { font: 10.5pt/1.42 -apple-system,Segoe UI,Helvetica,Arial,sans-serif; color:#111; margin:0; }
h1 { font-size:16pt; margin:0 0 1mm; }
.sub { color:#666; font-size:9pt; margin:0 0 7mm; }
.s { display:grid; grid-template-columns:72mm 1fr; gap:5mm;
     page-break-inside:avoid; break-inside:avoid;
     margin-bottom:6mm; padding-bottom:5mm; border-bottom:1px solid #ddd; }
.s img { width:100%; height:auto; border:1px solid #ccc; border-radius:3px; }
.n { font-size:8pt; color:#999; font-weight:700; letter-spacing:.08em; }
.k { font-size:7.5pt; color:#888; text-transform:uppercase; letter-spacing:.1em; margin-top:1mm; }
.t { font-size:11.5pt; font-weight:700; margin:1mm 0 2mm; }
.notes p { margin:0 0 2.2mm; }
.notes code { background:#f2f2f2; padding:0 2px; border-radius:2px; }
</style>
<h1>wingfoil — speaker notes</h1>
<p class="sub">${rows.length} slides · printed fallback for the reveal.js speaker view (press S)</p>
${rows.map(r => `<div class="s">
  <div><img src="${r.file}"></div>
  <div><div class="n">${r.i} / ${rows.length}</div>
    ${r.kicker?`<div class="k">${r.kicker}</div>`:''}
    <div class="t">${r.title||'&nbsp;'}</div>
    <div class="notes">${r.notes}</div></div>
</div>`).join('\n')}`);

  const b2 = await chromium.launch({ executablePath: '/opt/pw-browsers/chromium-1194/chrome-linux/chrome' });
  const p2 = await b2.newPage();
  await p2.goto('file:///tmp/pw/notes.html', { waitUntil:'load' });
  await p2.waitForTimeout(2000);
  await p2.pdf({ path:'/tmp/pw/speaker-notes.pdf', format:'A4', printBackground:true });
  await b2.close();
  console.log('built from', rows.length, 'slides');
})();
