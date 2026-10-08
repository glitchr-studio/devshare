// The window of the DevShare app. It displays what the app sends and asks
// for what is declared, add a project, remove one, share, disconnect,
// invite, stop; and, as a guest, join, leave, open an address of the joined
// session, and the invitation a link handed over.
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

function clock(seconds) {
  const minutes = Math.floor(seconds / 60);
  return `${String(minutes).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
}

function show(view) {
  $('setup').hidden = view !== 'setup';
  $('session').hidden = view !== 'session';
  $('joined').hidden = view !== 'joined';
}

// ---------------------------------------------------------- before sharing

async function setup(ended) {
  const declared = await invoke('declared');
  $('ended').hidden = !ended;
  $('ended').textContent = ended ? `The session is over: ${ended}.` : '';

  const none = declared.environments.length === 0;
  $('undeclared').hidden = !none;
  $('choose').hidden = none;
  $('path').textContent = declared.settings;
  $('problems').replaceChildren(...declared.problems.map((problem) =>
    element('li', {}, [
      element('span', { textContent: problem.folder ? `${problem.folder}: ${problem.text}` : problem.text }),
      ...(problem.folder ? [removal(problem.folder)] : []),
    ])));

  // The usual duration and number of guests come from the general settings.
  const minutes = String(declared.minutes);
  if (![...$('minutes').options].some((option) => option.value === minutes)) {
    const hours = declared.minutes % 60 === 0 ? declared.minutes / 60 : null;
    const label = hours ? `${hours} hour${hours > 1 ? 's' : ''}` : `${minutes} minutes`;
    $('minutes').append(element('option', { value: minutes, textContent: label }));
  }
  $('minutes').value = minutes;
  $('limit').value = declared.guests;

  $('environments').replaceChildren(...declared.environments.map((environment) => {
    const stopped = element('span', { className: 'stopped', hidden: true, textContent: 'Not started: nothing answers on its ports.' });
    const start = element('button', { type: 'button', textContent: 'Start', hidden: true, title: 'docker compose up -d in its folder' });
    start.addEventListener('click', () => startProject(environment.folder, start, stopped));
    const alone = element('button', { type: 'button', textContent: 'Share', title: `Share ${environment.name} alone` });
    alone.addEventListener('click', () => share([environment.name], alone));
    const row = element('li', {}, [
      element('label', {}, [
        element('input', { type: 'checkbox', name: 'environment', value: environment.name, checked: true }),
        element('span', {}, [
          element('strong', { textContent: environment.name }),
          element('span', { className: 'mono', textContent: environment.services.join('   ') }),
          element('span', { className: 'folder', textContent: environment.folder }),
          stopped,
        ]),
      ]),
      element('span', { className: 'row-actions' }, [start, alone, removal(environment.folder)]),
    ]);
    row.dataset.environment = environment.name;
    row.stopped = (isStopped) => { stopped.hidden = !isStopped; start.hidden = !isStopped; };
    return row;
  }));
  chosen();
  show('setup');
  refreshRunning();
  refreshCandidates();
  refreshComputer();
  loadSettings();
}

// Which projects answer on their ports: the others get a Start button.
async function refreshRunning() {
  const running = new Set(await invoke('running').catch(() => []));
  for (const row of $('environments').children) {
    row.stopped(!running.has(row.dataset.environment));
  }
}

async function startProject(folder, button, said) {
  button.disabled = true;
  button.textContent = 'Starting…';
  try {
    await invoke('start_project', { path: folder });
    said.textContent = 'Started.';
    await refreshRunning();
  } catch (error) {
    said.hidden = false;
    said.textContent = String(error);
  }
  button.disabled = false;
  button.textContent = 'Start';
}

// Projects found where projects are kept, one click from the list.
async function refreshCandidates() {
  const found = await invoke('candidates').catch(() => []);
  $('found').hidden = found.length === 0;
  $('candidates').replaceChildren(...found.map((candidate) => {
    const button = element('button', { type: 'button', textContent: 'Add' });
    button.addEventListener('click', () => add(candidate.folder));
    return element('li', {}, [
      element('div', { className: 'who' }, [
        element('strong', { textContent: candidate.name }),
        element('span', { className: 'mono muted', textContent: candidate.hostname ?? '' }),
        element('span', { className: 'folder', textContent: candidate.folder }),
      ]),
      button,
    ]);
  }));
}

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
    $('authority-state').textContent = 'None yet: HTTPS services of the sessions you join show the host\'s certificate.';
  } else if (!authority.trusted) {
    $('authority-state').textContent = `${authority.name}, not trusted by this computer yet.`;
  } else {
    const renew = authority.covers_domain ? '' : ' It was made before your domain was chosen: renew it to cover it.';
    $('authority-state').textContent = `Trusted: HTTPS services of the sessions you join open without a warning (${authority.domains.join(', ')}; ${authority.days_left} days left).${renew}`;
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

// The general settings, edited in place.
const SETTINGS = ['duration', 'guests', 'domain', 'relay', 'server', 'join'];

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
    await setup();
  } catch (error) {
    $('settings-said').className = 'error';
    $('settings-said').textContent = String(error);
  }
});

// Takes a project off the app's list. Its folder and its files stay.
function removal(folder) {
  const button = element('button', { type: 'button', textContent: 'Remove', title: 'Take this project off the list. Its folder is not touched.' });
  button.addEventListener('click', async () => {
    await invoke('remove_project', { path: folder });
    setup();
  });
  return button;
}

function chosen() {
  const names = [...document.querySelectorAll('input[name="environment"]:checked')].map((box) => box.value);
  $('share').disabled = names.length === 0;
  return names;
}

$('environments').addEventListener('change', chosen);
$('reload').addEventListener('click', () => setup());

// A project is added by its folder: typed, or dropped on the window.
async function add(folder) {
  if (!folder || !$('session').hidden) return;
  $('added').className = 'muted';
  $('added').textContent = 'Reading the project…';
  try {
    const project = await invoke('add_project', { path: folder });
    await setup();
    $('folder').value = '';
    $('added').textContent = `${project} was added.`;
  } catch (error) {
    $('added').className = 'error';
    $('added').textContent = String(error);
  }
}

$('add').addEventListener('submit', (event) => {
  event.preventDefault();
  add($('folder').value);
});
listen('tauri://drag-drop', (event) => add(event.payload.paths[0]));

// Shares these environments: the ones ticked, or one project alone.
async function share(environments, button) {
  $('error').textContent = '';
  const label = button.textContent;
  button.disabled = true;
  button.textContent = 'Starting…';
  try {
    await invoke('share', {
      environments,
      minutes: Number($('minutes').value),
      guests: Number($('limit').value),
    });
  } catch (error) {
    $('error').textContent = String(error);
  }
  button.textContent = label;
  button.disabled = false;
  chosen();
}

$('choose').addEventListener('submit', (event) => {
  event.preventDefault();
  share(chosen(), $('share'));
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

  $('shared-names').textContent = session.environments.map((environment) => environment.name).join(', ');
  $('remaining').textContent = clock(session.remaining);
  $('remaining').parentElement.classList.toggle('soon', session.remaining < 60);

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
    $('shared').replaceChildren(...session.environments.flatMap((environment) => [
      element('h3', { textContent: environment.name }),
      element('ul', {}, environment.services.map((service) =>
        element('li', { className: 'mono' }, [
          service.address,
          ...(service.tls ? [element('span', { className: 'tag', textContent: 'TLS' })] : []),
          ...(service.warning ? [element('span', { className: 'warning', textContent: service.warning })] : []),
        ]))),
    ]));
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
  try {
    await invoke('join', { invitation });
    $('join-invitation').value = '';
    $('handed').hidden = true;
  } catch (error) {
    $('join-error').textContent = String(error);
  }
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
  $('joined-remaining').textContent = clock(view.remaining);
  $('joined-remaining').parentElement.classList.toggle('soon', view.remaining < 60);
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

listen('session', (event) => render(event.payload));
listen('ended', (event) => {
  forget();
  setup(event.payload);
});

setup().then(() => invoke('handed')).then(handed);
