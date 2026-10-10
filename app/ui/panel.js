// The panel of the menu bar icon. It asks the same app as the window: the
// projects, starting and stopping them, sharing; and it opens the window
// for the rest.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow, LogicalSize } = window.__TAURI__.window;

const $ = (id) => document.getElementById(id);
const LABEL = 'panel';
const UNLIMITED = 5 * 365 * 24 * 3600;

function element(tag, properties = {}, children = []) {
  const node = Object.assign(document.createElement(tag), properties);
  node.append(...children);
  return node;
}

function clock(seconds) {
  if (seconds > UNLIMITED) return 'no time limit';
  const minutes = Math.floor(seconds / 60);
  return `${String(minutes).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
}

let projects = [];
let minutes = 5;
let guests = 3;
let session = null;

async function refresh() {
  let overview;
  try {
    overview = await invoke('overview');
  } catch (error) {
    say(error);
    return;
  }
  projects = overview.projects;
  minutes = overview.minutes;
  guests = overview.guests;
  $('empty').hidden = projects.length > 0;
  $('cards').replaceChildren(...projects.map(card));
  bar();
  fit();
  running();
}

// The switches start and stop the projects, as in the window; sharing is
// only the Share button's.
let states = new Map();
const pending = new Map();

const isUp = (project) => ['running', 'partial'].includes(states.get(project.folder));

function card(project) {
  const toggle = element('input', { type: 'checkbox', className: 'switch' });
  toggle.addEventListener('change', () => toggleRunning(project, toggle.checked));
  const dot = element('span', { className: 'dot' });
  const who = element('span', { className: 'who' }, [
    element('strong', { textContent: project.name }),
    element('span', { textContent: project.hostname ?? '', className: project.hostname ? '' : 'sub' }),
  ]);
  // The site, in the browser: the button, or a double click on the card.
  const site = element('button', { type: 'button', className: 'tool site', title: 'Open the site', ariaLabel: 'Open the site' });
  site.innerHTML = '<svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true"><path d="M6.5 3.5H3.5v9h9v-3M9 2.5h4.5V7M13.5 2.5 7.5 8.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>';
  site.addEventListener('click', () => openSite(project));
  // One click on the name opens its page in the window, unless a second
  // click follows: then the site opens.
  let clicked = null;
  who.addEventListener('click', () => {
    clearTimeout(clicked);
    clicked = setTimeout(() => open(`project:${project.folder}`), 260);
  });
  const row = element('li', { className: 'card' }, [dot, who, site, toggle]);
  row.addEventListener('dblclick', (event) => {
    if (event.target.closest('input, button')) return;
    clearTimeout(clicked);
    openSite(project);
  });
  row.dataset.folder = project.folder;
  row.update = () => {
    const state = states.get(project.folder);
    const moving = pending.has(project.folder);
    dot.className = `dot ${project.problem || state === 'partial' ? 'warn' : state === 'running' ? 'on' : 'off'}`;
    dot.title = project.problem ?? { running: 'Running', partial: 'Partly running: not every port answers' }[state] ?? 'Not started';
    site.hidden = !isUp(project) || !project.preview;
    toggle.checked = moving ? pending.get(project.folder) : isUp(project);
    toggle.disabled = moving || !project.startable || Boolean(project.problem);
    toggle.classList.toggle('busy', moving);
    toggle.classList.toggle('partial', !moving && state === 'partial');
    toggle.title = project.problem ?? (!project.startable ? 'No start command known: see its page' : isUp(project) ? 'Running: switch off to stop it' : 'Stopped: switch on to start it');
  };
  row.update();
  return row;
}

async function openSite(project) {
  try {
    await invoke('open_project', { path: project.folder });
    await invoke('hide_panel');
  } catch (error) {
    say(error);
  }
}

async function toggleRunning(project, on) {
  if (pending.has(project.folder)) return;
  pending.set(project.folder, on);
  update();
  invoke('switch', { path: project.folder, on }).catch(() => {});
  try {
    await invoke('run_project', { path: project.folder, action: on ? 'up' : 'down' });
  } catch (error) {
    say(`${project.name}: ${error}`);
  }
  pending.delete(project.folder);
  await running();
}

function update() {
  for (const row of $('cards').children) row.update();
  bar();
}

async function running() {
  const folders = projects.map((project) => project.folder);
  const found = await invoke('running', { paths: folders }).catch(() => []);
  states = new Map(found.map((one) => [one.path, one.state]));
  update();
}

// The Share button and what is said next to it.
function bar() {
  const chosen = projects.filter((project) => isUp(project) && !project.problem);
  if (session) {
    $('share').textContent = 'Stop sharing';
    $('share').className = 'primary stop';
    $('share').disabled = false;
    $('left').textContent = `${session.guests.length} connected · ${clock(session.remaining)}`;
    $('state').textContent = 'Sharing';
    $('state').className = 'state on';
  } else {
    $('share').textContent = chosen.length > 1 ? `Share ${chosen.length} running projects` : chosen.length ? `Share ${chosen[0].name}` : 'Share';
    $('share').className = 'primary';
    $('share').disabled = chosen.length === 0;
    $('left').textContent = chosen.length ? (minutes === 0 ? 'no time limit' : `${minutes} min`) : 'start a project to share it';
    $('state').textContent = '';
    $('state').className = 'state';
  }
}

$('share').addEventListener('click', async () => {
  $('error').hidden = true;
  $('share').disabled = true;
  try {
    if (session) {
      await invoke('stop');
    } else {
      const paths = projects.filter((project) => isUp(project) && !project.problem).map((project) => project.folder);
      await invoke('share', { paths, minutes, guests });
    }
  } catch (error) {
    say(error);
    $('share').disabled = false;
  }
});

function say(error) {
  $('error').textContent = String(error);
  $('error').hidden = false;
  fit();
}

// The window, on a page of it.
async function open(page) {
  await invoke('show_window', { page });
  await invoke('hide_panel');
}

$('open').addEventListener('click', () => open('share'));
$('settings').addEventListener('click', () => open('settings'));
$('quit').addEventListener('click', () => invoke('quit'));

// The window is as tall as the panel.
async function fit() {
  const height = Math.ceil(document.querySelector('.panel').getBoundingClientRect().height) + 2;
  await getCurrentWindow().setSize(new LogicalSize(360, height)).catch(() => {});
}

listen('refresh', refresh);
listen('changed', (event) => { if (event.payload !== LABEL) refresh(); });
listen('session', (event) => {
  session = event.payload;
  bar();
});
listen('ended', () => {
  session = null;
  refresh();
});
refresh();
