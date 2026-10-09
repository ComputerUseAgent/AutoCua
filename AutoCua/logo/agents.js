// agents.html's script, a file of its own: as the extension's page the content security
// policy runs no inline script, and the video would stay invisible over the gradient
// (measured).
const bg = document.getElementById('bg');
// Shown once a frame is there, whichever event says so first ('playing' alone was
// missed, and the video stayed invisible over the gradient: measured).
const show = () => bg.classList.add('on');
for (const ev of ['loadeddata', 'canplay', 'playing', 'timeupdate']) bg.addEventListener(ev, show, { once: true });
if (bg.readyState >= 2) show();
if (matchMedia('(prefers-reduced-motion: reduce)').matches) bg.pause(); else bg.play().catch(() => {});
const n = parseInt(new URLSearchParams(location.search).get('n') || '0', 10);
if (n > 1) document.getElementById('count').textContent = n + ' agents, each working in a tab of its own';
