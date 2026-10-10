// The window of the DevShare app: the projects at the left, each with its
// switch and its page (addresses, preview, how it starts and stops); Share
// and join; the settings behind the cog. While sharing: the invitation, who
// is connected, disconnect, invite, stop. As a guest: join, leave, open an
// address of the joined session, and the invitation a link handed over.
//
// Names of guests come from their computers: they are only ever written as
// text, never as markup.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const PLATFORMS = { macos: 'macOS', linux: 'Linux', windows: 'Windows', ios: 'iOS', android: 'Android' };

function element(tag, properties = {}, children = []) {
  const node = Object.assign(document.createElement(tag), properties);
  node.append(...children);
  return node;
}

// Five years and more: a session without a time limit.
const UNLIMITED = 5 * 365 * 24 * 3600;

function clock(seconds) {
  if (seconds > UNLIMITED) return 'no time limit';
  const minutes = Math.floor(seconds / 60);
  return `${String(minutes).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
}

// `04:59 left`, or `No time limit`.
function left(seconds) {
  return seconds > UNLIMITED ? 'No time limit' : `${clock(seconds)} left`;
}

// The window: the projects at the left, one page at a time in the middle.
// "Share and join" shows the session while one runs.

const LABEL = 'main';
let listed = [];
let hiddenOnes = [];
let selected = null;
let page = 'share';
let sharingNow = false;
let joinedNow = false;
// Folder → `running`, `partial` or `stopped`.
let states = new Map();
// True while a join is under way: sharing waits for it.
let joining = false;

function show(view) {
  page = view;
  const main = view === 'share'
    ? (sharingNow ? 'session' : joinedNow ? 'joined' : 'page-share')
    : `page-${view}`;
  for (const node of document.querySelectorAll('.content > main')) node.hidden = node.id !== main;
  for (const row of document.querySelectorAll('.nav-row')) {
    const mine = row.dataset.page === view && (view !== 'project' || row.dataset.folder === selected);
    row.classList.toggle('selected', mine);
  }
  // A preview only loads while its page is shown.
  if (view !== 'project') $('preview').removeAttribute('src');
  if (view === 'settings') {
    refreshComputer();
    loadSettings();
  }
}

// ---------------------------------------------------------- the projects

async function setup(ended) {
  $('ended').hidden = !ended;
  $('ended').textContent = ended ? `The session is over: ${ended}.` : '';
  show('share');
  await refreshProjects();
}

async function refreshProjects() {
  let overview;
  try {
    overview = await invoke('overview');
  } catch (error) {
    $('error').textContent = String(error);
    return;
  }
  listed = overview.projects;
  hiddenOnes = overview.hidden;
  showSources(overview.folders);
  if (overview.problem) $('error').textContent = overview.problem;

  $('none').hidden = listed.length > 0;
  $('projects').replaceChildren(...listed.map(projectRow));
  const button = $('show-hidden');
  button.hidden = hiddenOnes.length === 0;
  button.textContent = `${hiddenOnes.length} not listed`;
  if (page === 'hidden') renderHidden();

  // The usual duration and number of guests come from the general settings.
  const minutes = String(overview.minutes);
  if (![...$('minutes').options].some((option) => option.value === minutes)) {
    const hours = overview.minutes % 60 === 0 ? overview.minutes / 60 : null;
    const label = hours ? `${hours} hour${hours > 1 ? 's' : ''}` : `${minutes} minutes`;
    $('minutes').append(element('option', { value: minutes, textContent: label }));
  }
  $('minutes').value = minutes;
  $('limit').value = overview.guests;

  const current = listed.find((project) => project.folder === selected);
  if (current) {
    renderProject(current);
  } else if (page === 'project') {
    selected = null;
    show('share');
  }
  chosen();
  show(page);
  await refreshRunning();
}

// `/Users/me/Sites` as `~/Sites`.
function home(folder) {
  return folder.replace(/^\/Users\/[^/]+/, '~').replace(/^\/home\/[^/]+/, '~');
}

// The switches start and stop a project: what they show is whether it runs
// (all its ports or some answer), whoever started it. Sharing is never
// theirs: only a Share button shares.
const pending = new Map(); // folder → the position it is being moved to

function isUp(project) {
  const state = states.get(project.folder);
  return state === 'running' || state === 'partial';
}

function switchFor(project) {
  const toggle = element('input', { type: 'checkbox', className: 'switch' });
  toggle.dataset.folder = project.folder;
  toggle.addEventListener('change', () => toggleRunning(project, toggle.checked));
  setSwitch(toggle, project);
  return toggle;
}

function setSwitch(toggle, project) {
  const moving = pending.has(project.folder);
  toggle.checked = moving ? pending.get(project.folder) : isUp(project);
  toggle.disabled = moving || !project.startable || Boolean(project.problem);
  toggle.classList.toggle('busy', moving);
  toggle.classList.toggle('partial', !moving && states.get(project.folder) === 'partial');
  toggle.title = project.problem
    ? project.problem
    : !project.startable
      ? 'DevShare does not know how to start it: start it as you usually do, or give it a start command on its page'
      : states.get(project.folder) === 'partial' ? 'Partly running: switch off to stop it, or Restart it on its page'
        : isUp(project) ? 'Running: switch off to stop it' : 'Stopped: switch on to start it';
}

function syncSwitches(project) {
  for (const toggle of document.querySelectorAll(`input.switch[data-folder="${CSS.escape(project.folder)}"]`)) setSwitch(toggle, project);
  if (project.folder === selected) projectState(project);
}

// Starts or stops a project its own way (make up, docker compose up -d…).
async function toggleRunning(project, on) {
  if (pending.has(project.folder) || !project.startable) return;
  pending.set(project.folder, on);
  syncSwitches(project);
  // Remembered: the names of the projects switched on point at this Mac.
  project.on = on;
  invoke('switch', { path: project.folder, on }).catch(() => {});
  let failed = null;
  try {
    await invoke('run_project', { path: project.folder, action: on ? 'up' : 'down' });
  } catch (error) {
    failed = String(error);
  }
  pending.delete(project.folder);
  await refreshRunning();
  syncSwitches(project);
  if (failed) {
    if (project.folder === selected) {
      $('project-problem').hidden = false;
      $('project-problem').textContent = failed;
    } else {
      $('error').textContent = `${project.name}: ${failed}`;
    }
  }
  if (project.folder === selected) checkProject(project);
}

function projectRow(project) {
  const dot = element('span', { className: 'dot' });
  const row = element('li', { className: 'nav-row' }, [
    switchFor(project),
    element('span', { className: 'nav-main' }, [
      element('strong', { textContent: project.name }),
      element('span', { className: 'nav-sub mono', textContent: project.hostname ?? (project.problem ? 'cannot be shared' : '') }),
    ]),
    dot,
  ]);
  row.dataset.page = 'project';
  row.dataset.folder = project.folder;
  row.addEventListener('click', (event) => {
    if (event.target.closest('input')) return;
    selected = project.folder;
    renderProject(project);
    show('project');
  });
  row.running = (state) => {
    dot.className = `dot ${project.problem || state === 'partial' ? 'warn' : state === 'running' ? 'on' : ''}`;
    dot.title = project.problem ?? STATE_WORDS[state];
  };
  row.running(states.get(project.folder));
  return row;
}

const STATE_WORDS = { running: 'Running', partial: 'Partly running: not every port answers', stopped: 'Not started' };

// Which projects answer on their ports: all of them (green), some (amber),
// none (grey).
async function refreshRunning() {
  const folders = listed.map((project) => project.folder);
  const found = await invoke('running', { paths: folders }).catch(() => []);
  states = new Map(found.map((one) => [one.path, one.state]));
  for (const row of $('projects').children) row.running(states.get(row.dataset.folder));
  for (const project of listed) syncSwitches(project);
  chosen();
}
setInterval(() => { if (!document.hidden) refreshRunning(); }, 8000);

// ------------------------------------------------------------ one project

function renderProject(project) {
  $('project-name').textContent = project.name;
  $('project-host').textContent = project.hostname ?? '';
  $('project-switch').dataset.folder = project.folder;
  setSwitch($('project-switch'), project);
  $('project-problem').hidden = !project.problem;
  $('project-problem').textContent = project.problem ?? '';
  $('project-folder').textContent = home(project.folder);
  // What it answered last time stays shown until the new answers come:
  // "checking…" only for a project never checked yet.
  const known = lastChecks.get(project.folder);
  if (known) {
    showChecks(project, known.checks);
  } else {
    $('project-addresses').replaceChildren(...project.addresses.map((address) =>
      element('li', {}, [
        element('span', { className: 'dot' }),
        element('span', { className: 'name mono', textContent: `${address.host}:${address.port}` }),
        element('span', { className: 'local muted', textContent: 'checking…' }),
      ])));
    $('certify-section').hidden = true;
    $('names-note').hidden = true;
  }
  if (!known || Date.now() - known.at > 5000) checkProject(project);
  const form = $('project-commands').elements;
  form.up.value = project.local_up ?? '';
  form.up.placeholder = project.usual_up ?? 'nothing known: say how';
  form.down.value = project.local_down ?? '';
  form.down.placeholder = project.usual_down ?? 'nothing known: say how';
  $('project-commands-said').textContent = '';
  projectState(project);
}

function projectState(project) {
  $('project-share-now').textContent = sharingNow ? 'Show invitation' : 'Share now';
  $('project-share-now').disabled = !sharingNow && (Boolean(project.problem) || !isUp(project));
  $('project-share-now').title = sharingNow || isUp(project) ? '' : 'Start it first: switch it on';
  const state = states.get(project.folder) ?? 'stopped';
  const moving = pending.has(project.folder);
  $('project-state').textContent = project.problem ? '' : moving ? (pending.get(project.folder) ? 'Starting…' : 'Stopping…') : STATE_WORDS[state].split(':')[0];
  $('project-state').className = moving ? 'state' : state === 'running' ? 'state on' : state === 'partial' ? 'state partial' : 'state';
  $('project-restart').hidden = moving || state !== 'partial' || !project.startable || Boolean(project.problem);
}

$('project-restart').addEventListener('click', async () => {
  const project = currentProject();
  if (!project) return;
  await toggleRunning(project, false);
  await toggleRunning(project, true);
});

// Every address of the project, asked as a guest would; the preview shows
// the page when one answers with a page, and says why otherwise.
// The last answers of each project, and when they came.
const lastChecks = new Map();
let checking = 0;
async function checkProject(project) {
  const mine = ++checking;
  let checks;
  try {
    checks = await invoke('check_project', { path: project.folder });
  } catch (error) {
    checks = [];
  }
  lastChecks.set(project.folder, { checks, at: Date.now() });
  if (mine !== checking || selected !== project.folder) return;
  showChecks(project, checks);
}

function showChecks(project, checks) {
  // One row per port: the names that share it said together, when they
  // got the same answer.
  const rows = [];
  for (const check of checks) {
    const same = rows.find((row) => row.port === check.port && row.detail === check.detail && row.state === check.state);
    if (same) same.hosts.push(check.host);
    else rows.push({ ...check, hosts: [check.host] });
  }
  $('project-addresses').replaceChildren(...rows.map((row) => {
    const usable = ['ok', 'warn', 'error'].includes(row.state);
    const open = element('button', { type: 'button', className: 'small', textContent: 'Open', disabled: !usable });
    open.addEventListener('click', () => invoke('open_local', { url: row.url }).catch((error) => { $('project-problem').hidden = false; $('project-problem').textContent = String(error); }));
    const dot = { ok: 'on', open: 'open', warn: 'warn', error: 'bad', taken: 'taken' }[row.state] ?? '';
    const tone = { warn: 'attention', taken: 'attention', error: 'danger' }[row.state] ?? 'muted';
    // The address, then what it answered, under it.
    return element('li', {}, [
      element('span', { className: `dot ${dot}`, title: row.detail }),
      element('span', { className: 'address' }, [
        element('span', { className: 'name mono', textContent: `${row.hosts.join(', ')}:${row.port}`, title: row.hosts.join('\n') }),
        element('span', { className: `said ${tone}`, textContent: row.state === 'taken' ? `${row.detail}: free it before starting the project` : row.detail }),
      ]),
      open,
    ]);
  }));
  preview(project, checks);
  certifyOffer(project, checks);
  namesNote(checks);
}

// The project's names open on this Mac once the helper points them at it;
// until then the addresses open on localhost, and this says why.
function namesNote(checks) {
  const unnamed = [...new Set(checks
    .filter((check) => check.host !== 'localhost' && !new URL(check.url).hostname.endsWith(check.host))
    .map((check) => check.host))];
  $('names-note').hidden = unnamed.length === 0;
  if (unnamed.length) {
    $('names-text').textContent = `${unnamed.join(', ')} ${unnamed.length > 1 ? 'do' : 'does'} not lead to this Mac yet, so Open uses localhost. DevShare's helper points the names of your projects at this Mac: it needs installing, or its update.`;
  }
}

$('names-fix').addEventListener('click', async () => {
  const button = $('names-fix');
  button.disabled = true;
  button.textContent = 'Updating…';
  try {
    await invoke('install_helper');
    // The names reach /etc/hosts in the background: asked again shortly.
    setTimeout(() => { const project = currentProject(); if (project) checkProject(project); }, 1500);
  } catch (error) {
    $('names-text').textContent = String(error);
  }
  button.disabled = false;
  button.textContent = 'Update the helper';
});

// HTTPS this Mac's browsers refuse: the project's own certificate, usually
// self-signed. This Mac's DevShare authority can issue it one they accept.
function certifyOffer(project, checks) {
  const refused = checks.filter((check) => check.tls && check.trusted === false);
  const stopped = checks.length > 0 && checks.every((check) => check.state === 'down' || check.state === 'taken');
  const https = (project.preview ?? '').startsWith('https');
  $('certify-section').hidden = refused.length === 0 && !(stopped && https);
  $('certify-said').textContent = '';
  $('certify-restart').hidden = true;
  if (refused.length) {
    const where = [...new Set(refused.map((check) => `${check.host}:${check.port}`))].join(', ');
    $('certify-text').textContent = `This Mac does not trust the certificate served on ${where}, by its name or by localhost: browsers warn, and the preview stays empty. This Mac's DevShare authority can issue the project one they accept for both, for its development names only.`;
  } else {
    $('certify-text').textContent = 'Its certificate cannot be checked while the project is stopped. If it is self-signed, this Mac\'s DevShare authority can issue it one the browsers accept, for its development names only.';
  }
}

// Checked again on demand, and by itself while the page is open.
$('check-again').addEventListener('click', () => { const project = currentProject(); if (project) checkProject(project); });
setInterval(() => {
  const project = currentProject();
  if (project && page === 'project' && !document.hidden) checkProject(project);
}, 15000);

$('certify').addEventListener('click', async () => {
  const project = currentProject();
  if (!project) return;
  const button = $('certify');
  button.disabled = true;
  button.textContent = 'Certifying…';
  $('certify-said').className = 'small';
  try {
    const done = await invoke('certify_project', { path: project.folder });
    const lines = [
      `Certified for ${done.names.join(', ')}: ${home(done.certificate)}.`,
      `The files it replaced are kept in ${home(done.kept)}.`,
    ];
    if (done.tracked) lines.push('The project\'s git tracks these files: they now show as changed. They are trusted on this Mac only: do not commit them.');
    const state = states.get(project.folder) ?? 'stopped';
    if (state !== 'stopped') {
      lines.push('Restart the project for it to serve the new certificate.');
      $('certify-restart').hidden = !project.startable;
    } else {
      lines.push('The project serves it from its next start.');
    }
    $('certify-said').textContent = lines.join('\n');
  } catch (error) {
    $('certify-said').className = 'small error';
    $('certify-said').textContent = String(error);
  }
  button.disabled = false;
  button.textContent = 'Certify with this Mac\'s authority';
});

$('certify-restart').addEventListener('click', async () => {
  const project = currentProject();
  if (!project) return;
  $('certify-restart').hidden = true;
  await toggleRunning(project, false);
  await toggleRunning(project, true);
});

function preview(project, checks) {
  const port = project.preview ? Number(new URL(project.preview).port || (project.preview.startsWith('https') ? 443 : 80)) : null;
  const pages = checks.filter((check) => check.page);
  const page = pages.find((check) => check.port === port) ?? pages[0];
  if (page) {
    if ($('preview').getAttribute('src') !== page.url) $('preview').src = page.url;
    $('preview-wrap').hidden = false;
    $('preview-note').className = 'muted small';
    $('preview-note').textContent = page.tls
      ? `${page.url} — if it stays blank, this Mac does not trust the project's certificate: Open it in the browser.`
      : page.url;
    return;
  }
  $('preview').removeAttribute('src');
  $('preview-wrap').hidden = true;
  // What each address answered is said under it: here, only that there
  // is nothing to show, and why in a word.
  $('preview-note').className = 'muted small';
  if (checks.length === 0) {
    $('preview-note').textContent = 'Nothing to preview: the project shares no address.';
  } else if (checks.every((check) => check.state === 'down' || check.state === 'taken')) {
    $('preview-note').textContent = project.startable
      ? 'Nothing answers: the project is not started. Start it to see it here.'
      : 'Nothing answers: start the project the way you usually do.';
  } else {
    $('preview-note').textContent = 'No address shows a page yet: see what each one answers below.';
  }
}

function currentProject() {
  return listed.find((project) => project.folder === selected);
}

$('project-switch').addEventListener('change', (event) => {
  const project = currentProject();
  if (project) toggleRunning(project, event.target.checked);
});
$('project-reveal').addEventListener('click', () => { if (selected) invoke('reveal', { path: selected }).catch(() => {}); });
// Share now: this project, alone, on demand.
$('project-share-now').addEventListener('click', () => {
  const project = currentProject();
  if (!project) return;
  if (sharingNow) {
    show('share');
    return;
  }
  share([project.folder], $('project-share-now'));
});
$('dock-share').addEventListener('click', () => {
  if (sharingNow) {
    show('share');
    return;
  }
  const folders = chosen();
  if (folders.length) share(folders, $('dock-share'));
});
$('minutes').addEventListener('change', chosen);
$('limit').addEventListener('change', chosen);
$('project-remove').addEventListener('click', async () => {
  if (!selected) return;
  await invoke('remove_project', { path: selected });
  selected = null;
  page = 'share';
  refreshProjects();
});
$('project-commands').addEventListener('submit', async (event) => {
  event.preventDefault();
  const project = currentProject();
  if (!project) return;
  const form = $('project-commands').elements;
  try {
    await invoke('set_commands', { path: project.folder, up: form.up.value.trim() || null, down: form.down.value.trim() || null });
    $('project-commands-said').textContent = 'Saved.';
    await refreshProjects();
  } catch (error) {
    $('project-commands-said').textContent = String(error);
  }
});


// ----------------------------------------------------------- not listed

function renderHidden() {
  $('hidden').replaceChildren(...hiddenOnes.map((project) => {
    const back = element('button', { type: 'button', className: 'small', textContent: 'Put back' });
    back.addEventListener('click', async () => {
      await invoke('restore_project', { path: project.folder });
      refreshProjects();
    });
    return element('li', {}, [
      element('strong', { textContent: project.name }),
      element('span', { className: 'folder', textContent: `${home(project.folder)} · ${project.why}` }),
      back,
    ]);
  }));
}

$('show-hidden').addEventListener('click', () => { renderHidden(); show('hidden'); });

// --------------------------------------------------------------- adding

function closeAdd() { $('add-choices').hidden = true; }
$('add-menu').addEventListener('click', (event) => {
  event.stopPropagation();
  $('add-choices').hidden = !$('add-choices').hidden;
});
document.addEventListener('click', closeAdd);
document.addEventListener('keydown', (event) => { if (event.key === 'Escape') closeAdd(); });
for (const choice of document.querySelectorAll('#add-choices button')) {
  choice.addEventListener('click', async () => {
    closeAdd();
    const kind = choice.dataset.add;
    const picked = await invoke('pick', { file: kind === 'file' }).catch(() => null);
    if (!picked) return;
    if (kind === 'source') {
      try {
        await invoke('add_source', { path: picked });
        refreshProjects();
      } catch (error) {
        $('error').textContent = String(error);
      }
    } else {
      add(picked);
    }
  });
}

// A project is added by its folder or its devshare.toml: picked, or dropped
// on the window. A folder holding projects becomes a folder to look in.
async function add(path) {
  if (!path || sharingNow) return;
  try {
    const name = await invoke('add_project', { path });
    await refreshProjects();
    if (!name.startsWith('projects in ')) {
      const added = listed.find((project) => project.name === name || project.folder.endsWith(`/${name}`));
      if (added) {
        selected = added.folder;
        renderProject(added);
        show('project');
      }
    }
  } catch (error) {
    $('error').textContent = String(error);
    show('share');
  }
}
listen('tauri://drag-drop', (event) => add(event.payload.paths[0]));

// ------------------------------------------------------------- sharing

// What the Share buttons share: the projects running now.
function chosen() {
  const running = listed.filter((project) => isUp(project) && !project.problem);
  $('share').disabled = running.length === 0 || joining;
  $('share').textContent = running.length > 1 ? `Share ${running.length} running projects` : running.length ? `Share ${running[0].name}` : 'Share';
  const dock = $('dock-share');
  if (sharingNow) {
    dock.textContent = 'Sharing · Show invitation';
    dock.className = 'primary sharing';
    dock.disabled = false;
    $('dock-note').textContent = 'QR code and link on the invitation page';
  } else {
    dock.textContent = running.length > 1 ? `Share ${running.length} running projects` : running.length ? `Share ${running[0].name}` : 'Share';
    dock.className = 'primary';
    dock.disabled = running.length === 0 || joining;
    $('dock-note').textContent = running.length ? `${$('minutes').selectedOptions[0]?.textContent ?? ''}, up to ${$('limit').value} guests` : 'Start a project to share it';
  }
  $('share-summary').textContent = running.length
    ? `Running now: ${running.map((project) => project.name).join(', ')}.`
    : 'No project is running: switch one on in the list at the left to start it.';
  $('nav-share-note').textContent = sharingNow ? 'Sharing' : joinedNow ? 'In a session' : running.length ? `${running.length} running` : '';
  return running.map((project) => project.folder);
}

async function share(paths, button) {
  $('error').textContent = '';
  const label = button.textContent;
  button.disabled = true;
  button.textContent = 'Starting…';
  try {
    await invoke('share', {
      paths,
      minutes: Number($('minutes').value),
      guests: Number($('limit').value),
    });
    // Straight to the invitation: its QR code and its link.
    sharingNow = true;
    show('share');
  } catch (error) {
    $('error').textContent = String(error);
    show('share');
  }
  button.textContent = label;
  button.disabled = false;
  chosen();
}

$('choose').addEventListener('submit', (event) => {
  event.preventDefault();
  const folders = chosen();
  if (folders.length) share(folders, $('share'));
});

// ------------------------------------------------------------- settings

function showSources(folders) {
  $('sources').replaceChildren(...folders.map((folder) => {
    const remove = element('button', { type: 'button', className: 'tool', textContent: '×', title: `Stop looking in ${home(folder)}` });
    remove.addEventListener('click', async () => {
      await invoke('remove_source', { path: folder }).catch((error) => { $('computer-error').textContent = String(error); });
      refreshProjects();
    });
    return element('li', {}, [element('span', { textContent: home(folder), title: folder }), remove]);
  }));
  if (folders.length === 0) $('sources').append(element('li', { className: 'muted', textContent: 'No folder: only the projects added by hand are listed.' }));
}

$('add-source').addEventListener('click', async () => {
  const folder = await invoke('pick', { file: false }).catch(() => null);
  if (!folder) return;
  try {
    await invoke('add_source', { path: folder });
    refreshProjects();
  } catch (error) {
    $('computer-error').textContent = String(error);
  }
});

// The helper and this device's certificate authority.
async function refreshComputer() {
  let computer;
  try {
    computer = await invoke('computer');
  } catch (error) {
    $('computer-error').textContent = String(error);
    return;
  }
  const ready = computer.helper === 'ready';
  $('helper-state').textContent = ready
    ? 'Installed: joining a session needs no administrator rights.'
    : computer.helper === 'absent'
      ? 'Not installed: joining a session needs it, once, with an administrator password.'
      : computer.helper;
  if (ready && computer.names) {
    $('helper-state').textContent = `Installed, but the projects' names could not be pointed at this Mac: ${computer.names}`;
  } else if (ready) {
    $('helper-state').textContent = 'Installed: joining a session needs no administrator rights, and the names of the projects switched on point at this Mac while DevShare runs.';
  }
  $('helper-install').hidden = ready && !computer.names;

  const authority = computer.authority;
  if (!authority) {
    $('authority-state').textContent = 'None yet: HTTPS services of the sessions you join show the host\'s own certificate.';
  } else if (!authority.trusted) {
    $('authority-state').textContent = `${authority.name}, not trusted by this computer yet.`;
  } else {
    const renew = authority.covers_domain ? '' : ' Made before your domain was chosen: renew it to cover that domain too.';
    $('authority-state').textContent = `Trusted: HTTPS services of the sessions you join open without a warning (${authority.days_left} days left).${renew}`;
  }
  $('authority-install').hidden = Boolean(authority && authority.trusted);
  $('authority-renew').hidden = !authority;
  $('authority-remove').hidden = !authority;
}

async function act(button, command, args) {
  $('computer-error').textContent = '';
  const label = button.textContent;
  button.disabled = true;
  button.textContent = 'Asking the system…';
  try {
    await invoke(command, args);
  } catch (error) {
    $('computer-error').textContent = String(error);
  }
  button.disabled = false;
  button.textContent = label;
  refreshComputer();
}

$('helper-install').addEventListener('click', (event) => act(event.currentTarget, 'install_helper', {}));
for (const action of ['install', 'renew', 'remove']) {
  $(`authority-${action}`).addEventListener('click', (event) => act(event.currentTarget, 'authority', { action }));
}

const SETTINGS = ['duration', 'guests', 'domain', 'up', 'down', 'relay', 'server', 'join'];

async function loadSettings() {
  try {
    const settings = await invoke('settings');
    for (const name of SETTINGS) {
      $('settings').elements[name].value = settings[name] ?? '';
    }
  } catch (error) {
    $('settings-said').textContent = String(error);
  }
}

$('settings').addEventListener('submit', async (event) => {
  event.preventDefault();
  const form = $('settings').elements;
  const values = Object.fromEntries(SETTINGS.map((name) => [name, form[name].value.trim() || null]));
  values.guests = values.guests ? Number(values.guests) : null;
  $('settings-said').className = 'muted';
  try {
    await invoke('save_settings', { values });
    $('settings-said').textContent = 'Saved.';
    await refreshProjects();
  } catch (error) {
    $('settings-said').className = 'error';
    $('settings-said').textContent = String(error);
  }
});

// ------------------------------------------------------------ navigation

$('nav-share').addEventListener('click', () => show('share'));
$('open-settings').addEventListener('click', () => show('settings'));
listen('settings', () => show('settings'));
listen('navigate', (event) => {
  const where = String(event.payload);
  if (where.startsWith('project:')) {
    selected = where.slice('project:'.length);
    const project = currentProject();
    if (project) renderProject(project);
    show(project ? 'project' : 'share');
  } else {
    show(where);
  }
});
// Changes made from the menu bar panel.
listen('changed', (event) => { if (event.payload !== LABEL) refreshProjects(); });
listen('trouble', (event) => { $('error').textContent = String(event.payload); show('share'); });

// The sidebar: open unless closed, remembered on this computer; toggled
// from its button or View › Show or Hide Sidebar (⌃⌘S).
if (/Mac/.test(navigator.platform)) document.documentElement.classList.add('mac');

function remembered() {
  try { return localStorage.getItem('sidebar') !== 'closed'; } catch (error) { return true; }
}

function sidebar(open) {
  document.body.classList.toggle('no-sidebar', !open);
  try { localStorage.setItem('sidebar', open ? 'open' : 'closed'); } catch (error) { /* not kept */ }
}

$('toggle-sidebar').addEventListener('click', () => sidebar(document.body.classList.contains('no-sidebar')));
listen('sidebar', () => sidebar(document.body.classList.contains('no-sidebar')));
sidebar(remembered());

// ----------------------------------------------------------- while sharing

let current = null;
const rows = new Map(); // guest id → its row
const seen = new Map(); // notice id → when the window first saw it

function guestRow(guest) {
  const login = element('div', { className: 'login' });
  const where = element('div', { className: 'where' });
  const button = element('button', { type: 'button', textContent: 'Disconnect' });

  // Two clicks: the first one asks, and forgets after a few seconds.
  let asked = null;
  button.addEventListener('click', async () => {
    if (!asked) {
      button.textContent = 'Disconnect?';
      button.classList.add('confirm');
      asked = setTimeout(() => {
        asked = null;
        button.textContent = 'Disconnect';
        button.classList.remove('confirm');
      }, 4000);
      return;
    }
    clearTimeout(asked);
    button.disabled = true;
    await invoke('disconnect', { guest: guest.id });
  });

  const row = element('li', {}, [
    element('span', { className: 'dot' }),
    element('div', { className: 'who' }, [login, where]),
    button,
  ]);
  row.update = (guest) => {
    // The login when the guest's computer gave one, its name otherwise.
    login.textContent = guest.user ?? guest.computer;
    where.textContent = [
      guest.user ? guest.computer : null,
      PLATFORMS[guest.platform] ?? guest.platform,
      guest.route ? `${guest.route}, ${guest.latency} ms` : 'connecting',
      `connected ${clock(guest.connected)}`,
    ].filter(Boolean).join(' · ');
  };
  return row;
}

function render(session) {
  const first = current === null;
  const invitationChanged = first || current.code !== session.code;
  current = session;

  const names = session.environments.map((environment) => environment.name);
  $('shared-names').textContent = names.length > 3 ? `${names.length} projects` : names.join(', ');
  $('remaining').textContent = left(session.remaining);
  $('remaining').classList.toggle('soon', session.remaining < 60);

  $('invitation').hidden = !session.invitation_open;
  $('reach').hidden = !session.invitation_open;
  $('reach').textContent = session.anywhere
    ? 'The link and the QR code work from any network.'
    : 'No relay is in use: this invitation only works on this network.';
  $('withdrawn').hidden = session.invitation_open;
  if (invitationChanged) {
    $('code').textContent = session.code;
    // Drawn by the app itself from the link: the only markup taken as is.
    $('qr').innerHTML = session.qr;
    $('invite').disabled = false;
  }

  if (first) {
    // One line per project: its first name, and what does not answer, said
    // once; every address on demand.
    $('shared').replaceChildren(...session.environments.map((environment) => {
      const hosts = [...new Set(environment.services.map((service) => service.address.replace(/:\d+$/, '')))];
      const silent = environment.services.filter((service) => service.warning);
      const note = silent.length === environment.services.length
        ? 'not started'
        : silent.length ? `${silent.length} of ${environment.services.length} addresses do not answer` : '';
      return element('details', {}, [
        element('summary', {}, [
          element('strong', { textContent: environment.name }),
          element('span', { className: 'mono muted', textContent: ` ${hosts[0] ?? ''}${hosts.length > 1 ? ` +${hosts.length - 1}` : ''}` }),
          ...(note ? [element('span', { className: 'warning', textContent: note })] : []),
        ]),
        element('ul', {}, environment.services.map((service) =>
          element('li', { className: 'mono' }, [
            service.address,
            ...(service.tls ? [element('span', { className: 'tag', textContent: 'TLS' })] : []),
            ...(service.warning ? [element('span', { className: 'warning', textContent: service.warning })] : []),
          ]))),
      ]);
    }));
  }

  $('count').textContent = `${session.guests.length} of ${session.max_guests}`;
  $('nobody').hidden = session.guests.length > 0;
  const present = new Set(session.guests.map((guest) => guest.id));
  for (const [id, row] of rows) {
    if (!present.has(id)) {
      row.remove();
      rows.delete(id);
    }
  }
  for (const guest of session.guests) {
    if (!rows.has(guest.id)) {
      rows.set(guest.id, guestRow(guest));
      $('connected').append(rows.get(guest.id));
    }
    rows.get(guest.id).update(guest);
  }

  for (const notice of session.notices) {
    if (!seen.has(notice.id)) seen.set(notice.id, new Date());
  }
  $('notices').replaceChildren(...session.notices.map((notice) =>
    element('li', { className: notice.warning ? 'warning' : '' }, [
      element('time', { textContent: seen.get(notice.id).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) }),
      element('span', { textContent: notice.text }),
    ])));

  sharingNow = true;
  chosen();
  if (page === 'share') show('share');
}

function forget() {
  current = null;
  rows.clear();
  seen.clear();
  $('connected').replaceChildren();
  $('notices').replaceChildren();
}

async function copy(button, text) {
  await navigator.clipboard.writeText(text);
  const label = button.textContent;
  button.textContent = 'Copied';
  setTimeout(() => { button.textContent = label; }, 1200);
}

$('copy-link').addEventListener('click', (event) => copy(event.currentTarget, current.link));
$('copy-code').addEventListener('click', (event) => copy(event.currentTarget, current.code));
$('invite').addEventListener('click', async () => {
  $('invite').disabled = true;
  try {
    await invoke('invite');
  } catch (error) {
    $('invite').disabled = false;
  }
});
$('stop').addEventListener('click', () => invoke('stop'));

// ------------------------------------------------------------ as a guest

// An invitation from a link is only ever shown: joining is the owner's
// click, never the page's.
function handed(invitation) {
  if (!invitation || joinedNow || sharingNow) return;
  show('share');
  $('join-invitation').value = invitation;
  $('handed').hidden = false;
  $('join-error').textContent = '';
  $('join').classList.add('primary');
  $('join').focus();
}

$('join-invitation').addEventListener('input', () => { $('handed').hidden = true; });

$('join-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const invitation = $('join-invitation').value.trim();
  if (!invitation) return;
  $('join-error').textContent = '';
  $('join').disabled = true;
  $('join').textContent = 'Joining…';
  joining = true;
  chosen();
  try {
    await invoke('join', { invitation });
    $('join-invitation').value = '';
  } catch (error) {
    $('join-error').textContent = String(error);
  }
  // A handed invitation is used once: what failed is not offered again.
  $('handed').hidden = true;
  joining = false;
  chosen();
  $('join').disabled = false;
  $('join').textContent = 'Join';
  $('join').classList.remove('primary');
});

function opener(url, label) {
  const button = element('button', { type: 'button', textContent: label });
  button.addEventListener('click', () => invoke('open', { url }).catch(() => {}));
  return button;
}

let joinedOnce = false;

function renderJoined(view) {
  $('joined-names').textContent = view.environments.map((environment) => environment.name).join(', ');
  $('joined-remaining').textContent = left(view.remaining);
  $('joined-remaining').classList.toggle('soon', view.remaining < 60);
  $('joined-route').textContent = view.route
    ? `Connected, ${view.route}, ${view.latency} ms.`
    : 'Connecting…';

  // What is shared does not change during a session.
  if (!joinedOnce) {
    joinedOnce = true;
    $('joined-environments').replaceChildren(...view.environments.flatMap((environment) => [
      element('h3', {}, [
        element('span', { textContent: environment.name }),
        ...(environment.entrypoint ? [opener(environment.entrypoint, 'Open')] : []),
      ]),
      element('ul', {}, [
        ...environment.services.map((service) =>
          element('li', {}, [
            element('span', { className: 'mono', textContent: service.address }),
            ...(service.kind
              ? [element('span', { className: 'tag', textContent: service.kind })]
              : service.certified
                ? [element('span', { className: 'tag certified', textContent: 'HTTPS, certified by this computer' })]
                : service.sha256 ? [element('span', { className: 'tag', textContent: 'HTTPS' })] : []),
            // Not the web: shown, for the program that speaks it.
            ...(service.kind ? [] : [opener(service.url, 'Open')]),
          ])),
        // Ways to open it with another program: this computer is in the
        // session whole, so that program reaches it by the name.
        ...environment.launches.map((launch) =>
          element('li', {}, [
            element('span', { className: 'mono', textContent: launch.url }),
            element('span', { className: 'tag', textContent: `open with ${launch.kind}` }),
          ])),
      ]),
    ]));
  }
  joinedNow = true;
  chosen();
  if (page === 'share') show('share');
}

$('leave').addEventListener('click', () => invoke('leave'));

listen('joined', (event) => renderJoined(event.payload));
listen('left', (event) => {
  joinedNow = false;
  joinedOnce = false;
  $('joined-environments').replaceChildren();
  setup(event.payload);
});
listen('invitation', (event) => handed(event.payload));

// Changes made from the menu bar icon.
listen('trouble', (event) => { $('error').textContent = String(event.payload); });

listen('session', (event) => render(event.payload));
listen('ended', (event) => {
  sharingNow = false;
  forget();
  setup(event.payload);
});

setup().then(() => invoke('handed')).then(handed);
