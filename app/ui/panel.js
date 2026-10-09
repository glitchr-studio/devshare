// The panel of the menu bar icon. It asks the same app as the window: the
// projects, their switches, sharing and stopping; and it opens the window
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
  running();
  bar();
  fit();
}

function card(project) {
  const toggle = element('input', { type: 'checkbox', className: 'switch', checked: project.on, disabled: Boolean(project.problem) || Boolean(session) });
  toggle.title = session ? 'What is shared does not change during a session' : project.on ? 'Shared when you press Share' : 'Not shared';
  toggle.addEventListener('change', async () => {
    project.on = toggle.checked;
    bar();
    await invoke('switch', { path: project.folder, on: toggle.checked }).catch(say);
  });
  const dot = element('span', { className: 'dot' });
  const who = element('span', { className: 'who' }, [
    element('strong', { textContent: project.name }),
    element('span', { textContent: project.hostname ?? '', className: project.hostname ? '' : 'sub' }),
  ]);
  // The name opens the project's page in the window.
  who.addEventListener('click', () => open(`project:${project.folder}`));
  const row = element('li', { className: 'card' }, [dot, who, toggle]);
  row.dataset.folder = project.folder;
  row.running = (state) => {
    dot.className = `dot ${project.problem || state === 'partial' ? 'warn' : state === 'running' ? 'on' : 'off'}`;
    dot.title = project.problem ?? { running: 'Running', partial: 'Partly running: not every port answers' }[state] ?? 'Not started';
  };
  return row;
}

async function running() {
  const folders = projects.map((project) => project.folder);
  const found = await invoke('running', { paths: folders }).catch(() => []);
  const states = new Map(found.map((one) => [one.path, one.state]));
  for (const row of $('cards').children) row.running(states.get(row.dataset.folder));
}

// The Share button and what is said next to it.
function bar() {
  const chosen = projects.filter((project) => project.on && !project.problem);
  if (session) {
    $('share').textContent = 'Stop sharing';
    $('share').className = 'primary stop';
    $('share').disabled = false;
    $('left').textContent = `${session.guests.length} connected · ${clock(session.remaining)}`;
    $('state').textContent = 'Sharing';
    $('state').className = 'state on';
  } else {
    $('share').textContent = chosen.length > 1 ? `Share ${chosen.length} projects` : 'Share';
    $('share').className = 'primary';
    $('share').disabled = chosen.length === 0;
    $('left').textContent = chosen.length ? (minutes === 0 ? 'no time limit' : `${minutes} min`) : 'switch on a project';
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
      const paths = projects.filter((project) => project.on && !project.problem).map((project) => project.folder);
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
  for (const row of $('cards').children) row.querySelector('input').disabled = true;
  bar();
});
listen('ended', () => {
  session = null;
  refresh();
});
refresh();
