// The window of the DevShare app. Before sharing: the projects, each
// switched on or off, started or stopped; this computer's helper and
// certificate authority; the settings. While sharing: the invitation, who
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

function show(view) {
  $('setup').hidden = view !== 'setup';
  $('session').hidden = view !== 'session';
  $('joined').hidden = view !== 'joined';
}

// ---------------------------------------------------------- before sharing

// The projects as last listed, by folder.
let listed = [];
// True while a join is under way: sharing waits for it.
let joining = false;

async function setup(ended) {
  $('ended').hidden = !ended;
  $('ended').textContent = ended ? `The session is over: ${ended}.` : '';
  show('setup');
  await refreshProjects();
}

// One line per project, in the same order whatever is switched: name, the
// name guests use, whether it runs; the rest on demand.
async function refreshProjects() {
  let overview;
  try {
    overview = await invoke('overview');
  } catch (error) {
    $('error').textContent = String(error);
    return;
  }
  listed = overview.projects;
  showSources(overview.folders);
  if (overview.problem) $('error').textContent = overview.problem;
  $('none').hidden = listed.length > 0;
  $('projects').replaceChildren(...listed.map(projectRow));
  showHidden(overview.hidden);

  // The usual duration and number of guests come from the general settings.
  const minutes = String(overview.minutes);
  if (![...$('minutes').options].some((option) => option.value === minutes)) {
    const hours = overview.minutes % 60 === 0 ? overview.minutes / 60 : null;
    const label = hours ? `${hours} hour${hours > 1 ? 's' : ''}` : `${minutes} minutes`;
    $('minutes').append(element('option', { value: minutes, textContent: label }));
  }
  $('minutes').value = minutes;
  $('limit').value = overview.guests;
  chosen();
  refreshRunning();
}

// The folders projects are looked for in, in the sidebar, each removable.
function showSources(folders) {
  $('sources').replaceChildren(...folders.map((folder) => {
    const remove = element('button', { type: 'button', className: 'chip-remove', textContent: '×', title: `Stop looking in ${home(folder)}` });
    remove.addEventListener('click', async () => {
      await invoke('remove_source', { path: folder }).catch((error) => { $('error').textContent = String(error); });
      refreshProjects();
    });
    return element('li', {}, [element('span', { textContent: home(folder), title: folder }), remove]);
  }));
  if (folders.length === 0) $('sources').append(element('li', { className: 'muted', textContent: 'No folder: projects are only the ones added.' }));
}

$('add-source').addEventListener('click', async () => {
  const folder = await invoke('pick', { file: false }).catch(() => null);
  if (!folder) return;
  try {
    await invoke('add_source', { path: folder });
    refreshProjects();
  } catch (error) {
    $('error').textContent = String(error);
  }
});

// What is not listed: taken off, or found with nothing to share. Folded,
// each one click from coming back.
function showHidden(hidden) {
  const button = $('show-hidden');
  button.hidden = hidden.length === 0;
  button.textContent = `${hidden.length} not listed`;
  if (hidden.length === 0) $('hidden').hidden = true;
  $('hidden').replaceChildren(...hidden.map((project) => {
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

$('show-hidden').addEventListener('click', () => { $('hidden').hidden = !$('hidden').hidden; });

// `/Users/me/Sites` as `~/Sites`.
function home(folder) {
  return folder.replace(/^\/Users\/[^/]+/, '~').replace(/^\/home\/[^/]+/, '~');
}

function projectRow(project) {
  const toggle = element('input', { type: 'checkbox', className: 'switch', checked: project.on, disabled: Boolean(project.problem) });
  toggle.title = project.on ? 'Shared when you press Share' : 'Not shared';
  toggle.addEventListener('change', async () => {
    project.on = toggle.checked;
    row.running(row.isRunning);
    chosen();
    await invoke('switch', { path: project.folder, on: toggle.checked }).catch(() => {});
  });

  const state = element('span', { className: 'state' });
  const start = element('button', { type: 'button', className: 'small', textContent: 'Start', hidden: true });
  start.addEventListener('click', () => runProject(project, 'up', start, state));

  const more = element('button', { type: 'button', className: 'disclose', textContent: '›', title: 'Details' });
  const details = element('div', { className: 'details', hidden: true }, [
    element('span', { className: 'mono', textContent: project.names.join('  ') || '—' }),
    element('span', { className: 'muted', textContent: project.ports.length ? `ports ${project.ports.join(', ')}` : '' }),
    element('span', { className: 'folder', textContent: home(project.folder) }),
    ...(project.problem ? [element('span', { className: 'error', textContent: project.problem })] : []),
  ]);
  const stop = element('button', { type: 'button', className: 'small', textContent: 'Stop', hidden: true });
  stop.addEventListener('click', () => runProject(project, 'down', stop, state));
  details.append(stop);

  // How this project starts and stops on this computer, over what its
  // devshare.toml and the defaults say.
  const upField = element('input', { type: 'text', value: project.local_up ?? '', placeholder: project.usual_up ?? 'nothing known: say how', spellcheck: false });
  const downField = element('input', { type: 'text', value: project.local_down ?? '', placeholder: project.usual_down ?? 'nothing known: say how', spellcheck: false });
  const keep = element('button', { type: 'button', className: 'small', textContent: 'Save' });
  const kept = element('span', { className: 'muted' });
  keep.addEventListener('click', async () => {
    try {
      await invoke('set_commands', { path: project.folder, up: upField.value.trim() || null, down: downField.value.trim() || null });
      kept.textContent = 'Saved.';
      project.local_up = upField.value.trim() || null;
      project.startable = Boolean(project.local_up || project.usual_up);
      row.running(row.isRunning);
    } catch (error) {
      kept.textContent = String(error);
    }
  });
  details.append(element('div', { className: 'commands' }, [
    element('label', {}, ['Start with', upField]),
    element('label', {}, ['Stop with', downField]),
    element('span', { className: 'commands-actions' }, [keep, kept]),
  ]));
  // Any project can go, found or added, until it is put back.
  const remove = element('button', { type: 'button', className: 'small', textContent: 'Take off the list' });
  remove.addEventListener('click', async () => {
    await invoke('remove_project', { path: project.folder });
    refreshProjects();
  });
  details.append(remove);
  more.addEventListener('click', () => {
    details.hidden = !details.hidden;
    more.classList.toggle('open', !details.hidden);
  });

  const extra = project.names.length > 1 ? ` +${project.names.length - 1}` : '';
  const row = element('li', {}, [
    element('label', { className: 'line' }, [
      toggle,
      element('strong', { textContent: project.name }),
      element('span', { className: 'mono muted', textContent: project.hostname ? project.hostname + extra : '' }),
    ]),
    state,
    start,
    more,
    details,
  ]);
  row.dataset.folder = project.folder;
  // What runs is said for the projects switched on: the others are not
  // the owner's concern right now.
  row.isRunning = false;
  row.running = (running) => {
    row.isRunning = running;
    state.textContent = !project.on ? '' : running ? 'running' : 'not started';
    state.className = running ? 'state running' : 'state stopped';
    start.hidden = !project.on || running || !project.startable;
    stop.hidden = !running || !project.startable;
  };
  return row;
}

// Which projects answer on their ports.
async function refreshRunning() {
  const folders = listed.map((project) => project.folder);
  const running = new Set(await invoke('running', { paths: folders }).catch(() => []));
  for (const row of $('projects').children) {
    row.running(running.has(row.dataset.folder));
  }
}

// Starts or stops a project its own way (make up, docker compose up -d).
async function runProject(project, action, button, state) {
  const label = button.textContent;
  button.disabled = true;
  button.textContent = action === 'up' ? 'Starting…' : 'Stopping…';
  $('error').textContent = '';
  try {
    await invoke('run_project', { path: project.folder, action });
  } catch (error) {
    $('error').textContent = `${project.name}: ${error}`;
  }
  button.disabled = false;
  button.textContent = label;
  refreshRunning();
}

function chosen() {
  const folders = listed.filter((project) => project.on && !project.problem).map((project) => project.folder);
  $('share').disabled = folders.length === 0 || joining;
  $('share').textContent = folders.length > 1 ? `Share ${folders.length} projects` : 'Share';
  return folders;
}

// A project is added by its folder: picked, or dropped on the window.
async function add(folder) {
  if (!folder || !$('session').hidden) return;
  $('added').className = 'muted';
  $('added').textContent = 'Reading the project…';
  try {
    const project = await invoke('add_project', { path: folder });
    $('added').textContent = project.startsWith('projects in ') ? `Now looking for ${project}.` : `${project} was added and switched on.`;
    await refreshProjects();
  } catch (error) {
    $('added').className = 'error';
    $('added').textContent = String(error);
  }
}

$('add').addEventListener('click', async () => {
  const folder = await invoke('pick', { file: false }).catch(() => null);
  if (folder) add(folder);
});
$('add-file').addEventListener('click', async () => {
  const file = await invoke('pick', { file: true }).catch(() => null);
  if (file) add(file);
});
listen('tauri://drag-drop', (event) => add(event.payload.paths[0]));

$('choose').addEventListener('submit', async (event) => {
  event.preventDefault();
  const folders = chosen();
  if (folders.length === 0) return;
  $('error').textContent = '';
  const label = $('share').textContent;
  $('share').disabled = true;
  $('share').textContent = 'Starting…';
  try {
    await invoke('share', {
      paths: folders,
      minutes: Number($('minutes').value),
      guests: Number($('limit').value),
    });
  } catch (error) {
    $('error').textContent = String(error);
  }
  $('share').textContent = label;
  chosen();
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
  $('helper-install').hidden = ready;

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

// The sidebar: open unless closed, remembered on this computer; toggled
// from its button, View > Show or Hide Sidebar (⌃⌘S), opened by
// DevShare > Settings… (⌘,).
if (/Mac/.test(navigator.platform)) document.documentElement.classList.add('mac');

function remembered() {
  try { return localStorage.getItem('sidebar') !== 'closed'; } catch (error) { return true; }
}

function sidebar(open) {
  document.body.classList.toggle('no-sidebar', !open);
  try { localStorage.setItem('sidebar', open ? 'open' : 'closed'); } catch (error) { /* not kept */ }
  if (open) {
    refreshComputer();
    loadSettings();
  }
}

$('toggle-sidebar').addEventListener('click', () => sidebar(document.body.classList.contains('no-sidebar')));
listen('sidebar', (event) => sidebar(event.payload === 'open' || document.body.classList.contains('no-sidebar')));
sidebar(remembered());

// The general settings, edited in place.
const SETTINGS = ['duration', 'guests', 'domain', 'up', 'down', 'relay', 'server', 'join'];

async function loadSettings() {
  try {
    const settings = await invoke('settings');
    for (const name of SETTINGS) {
      const value = settings[name];
      $('settings').elements[name].value = Array.isArray(value) ? value.join(', ') : value ?? '';
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

  show('session');
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
  if (!invitation || !$('joined').hidden || !$('session').hidden) return;
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
      element('ul', {}, environment.services.map((service) =>
        element('li', {}, [
          element('span', { className: 'mono', textContent: service.address }),
          ...(service.certified
            ? [element('span', { className: 'tag certified', textContent: 'HTTPS, certified by this computer' })]
            : service.sha256 ? [element('span', { className: 'tag', textContent: 'HTTPS' })] : []),
          opener(service.url, 'Open'),
        ]))),
    ]));
  }
  show('joined');
}

$('leave').addEventListener('click', () => invoke('leave'));

listen('joined', (event) => renderJoined(event.payload));
listen('left', (event) => {
  joinedOnce = false;
  $('joined-environments').replaceChildren();
  setup(event.payload);
});
listen('invitation', (event) => handed(event.payload));

// Changes made from the menu bar icon.
listen('projects', () => { if (!$('setup').hidden) refreshProjects(); });
listen('trouble', (event) => { $('error').textContent = String(event.payload); });

listen('session', (event) => render(event.payload));
listen('ended', (event) => {
  forget();
  setup(event.payload);
});

setup().then(() => invoke('handed')).then(handed);
