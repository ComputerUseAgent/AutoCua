// AutoCua's desktop window: the app in AutoCua/ui, in Chromium.
//
// The Python backend (python -m AutoCua.ui.service --desktop) is a process of
// its own. It serves the UI on AutoCua's own port (APP_PORT in
// AutoCua/__init__.py; it reports the one it got) and streams what it used to
// push into the pywebview window (scripts to run in the page, window actions)
// on /api/desktop/events. So this window never shares a process, a thread or a
// lock with the agent, and it is the same Chromium on macOS, Windows and Linux.
//
//   npm install     once, in this folder
//   npm start       or run_agent(..., ui=True), which uses this when installed
'use strict';

// Started as plain Node: ELECTRON_RUN_AS_NODE=1 was inherited (VS Code, an
// Electron app, sets it for what it spawns), so there is no `app` here. Run
// again as Electron without it.
if (typeof require('electron') === 'string') {
  const env = { ...process.env };
  delete env.ELECTRON_RUN_AS_NODE;
  const result = require('node:child_process').spawnSync(
    process.execPath, process.argv.slice(1), { stdio: 'inherit', env });
  process.exit(result.status ?? 1);
}

const { app, BaseWindow, WebContentsView, nativeImage, nativeTheme, shell } = require('electron');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');
const readline = require('node:readline');

// This folder ships inside the AutoCua package (AutoCua/desktop). ROOT holds
// that package: the checkout, or site-packages after a pip install. The
// backend runs from ROOT, so `-m AutoCua...` loads this same copy of AutoCua.
const PACKAGE = path.resolve(__dirname, '..');
const ROOT = path.dirname(PACKAGE);
const BACKGROUND = '#FDFCFA';   // TITLEBAR_COLOR in AutoCua/ui/service.py
const LOGO = path.join(PACKAGE, 'logo', 'logo.png');
const PENDING_MAX = 500;
const MAC = process.platform === 'darwin';

// AUTOCUA_DESKTOP_SMOKE=<file.png>: a test run with nothing visible on screen. It
// waits out the splash, checks that a pushed script runs, saves a screenshot
// and a <file>.json report, and quits.
const SMOKE = process.env.AUTOCUA_DESKTOP_SMOKE || '';

let win = null;
let page = null;       // the app, in a view of the window
let titleBar = null;   // macOS: the strip above it, in the app's colour
let backend = null;
let quitting = false;
let relaunching = false;
let stopping = null;
let pageReady = false;
let eventsConnected = false;
const pending = [];   // scripts that arrived while no page was ready for them

function pythonExecutable() {
  if (process.env.AUTOCUA_PYTHON) return process.env.AUTOCUA_PYTHON;
  const venv = process.platform === 'win32'
    ? path.join(ROOT, '.venv', 'Scripts', 'python.exe')
    : path.join(ROOT, '.venv', 'bin', 'python');
  if (fs.existsSync(venv)) return venv;
  return process.platform === 'win32' ? 'python' : 'python3';
}

// Start the backend. Resolves with the app's URL once it is serving.
function startBackend() {
  return new Promise((resolve, reject) => {
    const child = spawn(pythonExecutable(), ['-m', 'AutoCua.ui.service', '--desktop'], {
      cwd: ROOT,
      env: { ...process.env, PYTHONUNBUFFERED: '1' },
      stdio: ['pipe', 'pipe', 'inherit'],
    });
    backend = child;
    let ready = false;
    readline.createInterface({ input: child.stdout }).on('line', (line) => {
      const match = /^AUTOCUA_DESKTOP_READY (\S+)$/.exec(line);
      if (match && !ready) {
        ready = true;
        resolve(match[1]);
      } else {
        process.stdout.write(line + '\n');   // the backend's own prints stay visible
      }
    });
    child.on('error', reject);
    child.on('exit', (code, signal) => {
      if (backend === child) backend = null;
      if (!ready) reject(new Error(`backend exited (${code ?? signal}) before it was ready`));
      else if (!quitting && !relaunching) app.quit();
    });
  });
}

// Closing its stdin asks the backend to run the window's closing hooks and
// exit; it is killed if it has not within a few seconds.
function stopBackend() {
  const child = backend;
  if (!child) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
    child.once('exit', () => {
      clearTimeout(timer);
      resolve();
    });
    child.stdin.end();
  });
}

// The backend's events, in the order it posted them: {type: 'js', code} runs
// in the page once a page is ready for it, {type: 'window', action} is for
// this window.
function subscribe(origin) {
  let retried = false;
  const retry = () => {
    if (retried) return;
    retried = true;
    eventsConnected = false;
    if (backend && !quitting && !relaunching) setTimeout(() => subscribe(origin), 500);
  };
  const request = http.get(origin + '/api/desktop/events', (response) => {
    if (response.statusCode !== 200) {
      response.resume();
      retry();
      return;
    }
    eventsConnected = true;
    response.setEncoding('utf8');
    let buffer = '';
    let scanFrom = 0;
    response.on('data', (chunk) => {
      buffer += chunk;
      let end;
      while ((end = buffer.indexOf('\n\n', scanFrom)) !== -1) {
        const block = buffer.slice(0, end);
        buffer = buffer.slice(end + 2);
        scanFrom = 0;
        const data = block.split('\n')
          .filter((line) => line.startsWith('data: '))
          .map((line) => line.slice(6))
          .join('\n');
        if (!data) continue;
        try {
          handle(JSON.parse(data));
        } catch (err) {
          console.error('[desktop] bad event:', err.message);
        }
      }
      scanFrom = Math.max(0, buffer.length - 1);   // a "\n\n" can span two chunks
    });
    response.on('close', retry);
  });
  request.on('error', retry);
}

function handle(event) {
  if (event.type === 'js') {
    if (win && pageReady) {
      run(event.code);
    } else {
      pending.push(event.code);
      if (pending.length > PENDING_MAX) pending.shift();
    }
    return;
  }
  if (event.type !== 'window' || !win) return;
  switch (event.action) {
    case 'minimize':
      win.minimize();
      break;
    case 'raise':
      if (win.isMinimized()) win.restore();
      win.show();
      app.focus({ steal: true });
      win.focus();
      page.webContents.focus();
      break;
    case 'close':
      win.close();
      break;
    case 'relaunch':
      relaunch();
      break;
  }
}

// Fire and forget, like the backend's evaluate_js: nothing waits on it.
function run(code) {
  if (!win) return;
  page.webContents.executeJavaScript(code).catch((err) => {
    console.error('[desktop] pushed script failed:', err && err.message);
  });
}

// The backend asked to be restarted (the permission wizard's restart, Reset
// everything): a fresh process re-reads what macOS has granted.
async function relaunch() {
  if (relaunching) return;
  relaunching = true;
  pageReady = false;
  pending.length = 0;
  await stopBackend();
  try {
    const url = await startBackend();
    relaunching = false;
    subscribe(new URL(url).origin);
    if (win) load(url);
  } catch (err) {
    console.error('[desktop] relaunch failed:', err.message);
    relaunching = false;
    app.quit();
  }
}

function load(url) {
  page.webContents.loadURL(url).catch((err) => {
    // ERR_ABORTED when the page navigates on its own while loading (the setup
    // wizard does); anything else is worth seeing.
    if (err.code !== 'ERR_ABORTED') console.error('[desktop] load failed:', err.message);
  });
}

// The height of the system title bar on this Mac (it differs between macOS
// versions): what a window with the standard bar loses to it.
function systemTitleBarHeight() {
  const probe = new BaseWindow({ show: false, width: 400, height: 300 });
  const height = probe.getSize()[1] - probe.getContentSize()[1];
  probe.destroy();
  return height > 0 ? height : 28;
}

// The logo fills its whole square, but a macOS icon keeps a clear margin round
// its shape (Apple's grid: an 824 px shape on a 1024 px canvas). Unpadded, it
// showed bigger than every other icon in the Dock.
function dockIcon() {
  const logo = nativeImage.createFromPath(LOGO);
  if (logo.isEmpty()) return null;
  const CANVAS = 1024;
  const SHAPE = 824;
  const at = (CANVAS - SHAPE) / 2;
  const shape = logo.resize({ width: SHAPE, height: SHAPE, quality: 'best' }).toBitmap();
  const out = Buffer.alloc(CANVAS * CANVAS * 4);   // transparent
  for (let y = 0; y < SHAPE; y++) {
    shape.copy(out, ((at + y) * CANVAS + at) * 4, y * SHAPE * 4, (y + 1) * SHAPE * 4);
  }
  return nativeImage.createFromBitmap(out, { width: CANVAS, height: CANVAS });
}

async function createWindow() {
  const url = await startBackend();
  win = new BaseWindow({
    width: 1140,
    height: 700,
    title: 'AutoCua',
    backgroundColor: BACKGROUND,
    show: false,
    acceptFirstMouse: true,   // macOS: the click that focuses the window lands too
    // macOS: no system bar (it can't take the app's colour); titleBar below
    // stands in for it, and the traffic lights stay where they were.
    ...(MAC ? { titleBarStyle: 'hidden' } : {}),
  });
  page = new WebContentsView({
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      // The agent minimises the window during a run; keep the page live.
      backgroundThrottling: false,
    },
  });
  page.setBackgroundColor(BACKGROUND);
  // The page gets the size it had under the system bar; the strip above it
  // is the app's colour and drags the window, so bar and page read as one
  // surface, like the pywebview window's tinted bar.
  let bar = 0;
  if (MAC) {
    bar = systemTitleBarHeight();
    titleBar = new WebContentsView({
      webPreferences: { contextIsolation: true, nodeIntegration: false, sandbox: true },
    });
    titleBar.setBackgroundColor(BACKGROUND);
    titleBar.webContents.loadURL('data:text/html,' + encodeURIComponent(
      `<style>html,body{margin:0;height:100%;background:${BACKGROUND};-webkit-app-region:drag}</style>`));
    win.contentView.addChildView(titleBar);
  }
  win.contentView.addChildView(page);
  const layout = () => {
    const [width, height] = win.getContentSize();
    const top = win.isFullScreen() ? 0 : bar;   // full screen has no bar
    if (titleBar) titleBar.setBounds({ x: 0, y: 0, width, height: top });
    page.setBounds({ x: 0, y: top, width, height: Math.max(0, height - top) });
  };
  layout();
  win.on('resize', layout);
  win.on('enter-full-screen', layout);
  win.on('leave-full-screen', layout);

  page.webContents.setWindowOpenHandler(({ url: target }) => {
    shell.openExternal(target);
    return { action: 'deny' };
  });
  page.webContents.on('did-navigate', () => {
    pageReady = false;
  });
  page.webContents.on('dom-ready', () => {
    pageReady = true;
    pending.splice(0).forEach(run);
    if (SMOKE) smokeCheck();
  });
  win.on('closed', () => {
    // A window's views keep their pages until they are closed themselves.
    for (const view of [page, titleBar]) {
      if (view && !view.webContents.isDestroyed()) view.webContents.close();
    }
    win = null;
  });
  subscribe(new URL(url).origin);
  load(url);
  if (SMOKE) {
    // Nothing to see: the window is invisible and takes no focus or clicks,
    // but it is drawn, which the page capture needs.
    win.setOpacity(0);
    win.setIgnoreMouseEvents(true);
    win.showInactive();
  } else {
    // Everything on screen is the app's colour until the splash paints, so
    // the window can show at once.
    win.show();
    page.webContents.focus();
  }
}

function smokeCheck() {
  if (smokeCheck.started) return;
  smokeCheck.started = true;
  // The splash hands off at 4 s and fades out over 0.9 s.
  setTimeout(() => {
    handle({ type: 'js', code: "document.title = 'AutoCua (pushed script ran)'" });
    setTimeout(async () => {
      const report = {
        url: page.webContents.getURL(),
        title: page.webContents.getTitle(),
        eventsConnected,
        electron: process.versions.electron,
        chrome: process.versions.chrome,
      };
      try {
        const image = await page.webContents.capturePage();
        fs.writeFileSync(SMOKE, image.toPNG());
      } catch (err) {
        report.captureError = err.message;
      }
      try {
        fs.writeFileSync(SMOKE + '.json', JSON.stringify(report, null, 2));
      } catch (err) {
        console.error('[desktop] smoke check failed:', err.message);
      }
      app.quit();
    }, 500);
  }, 5500);
}

if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on('second-instance', () => {
    if (!win) return;
    if (win.isMinimized()) win.restore();
    win.show();
    win.focus();
  });
  app.whenReady().then(() => {
    // The app is light only: keep the traffic lights and menus light on its
    // off-white bar in Dark Mode too, as the pywebview window pins Aqua.
    if (MAC) nativeTheme.themeSource = 'light';
    if (MAC && app.dock) {
      if (SMOKE) {
        app.dock.hide();
      } else {
        const icon = dockIcon();
        if (icon) app.dock.setIcon(icon);
      }
    }
    return createWindow();
  }).catch((err) => {
    console.error('[desktop] could not start:', err.message);
    app.quit();
  });
}

app.on('window-all-closed', () => app.quit());

// Quit only once the backend is gone, so its closing hooks have run.
app.on('before-quit', (event) => {
  if (!backend) return;
  event.preventDefault();
  quitting = true;
  if (!stopping) stopping = stopBackend().then(() => app.quit());
});
