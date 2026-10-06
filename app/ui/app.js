// The window of the DevShare host app. It displays what the app sends and
// asks for seven things: what is declared, add a project, remove one, share,
// disconnect, invite, stop.
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
    $('minutes').append(element('option', { value: minutes, textContent: `${minutes} minutes` }));
  }
  $('minutes').value = minutes;
  $('limit').value = declared.guests;

  $('environments').replaceChildren(...declared.environments.map((environment) =>
    element('li', {}, [
      element('label', {}, [
        element('input', { type: 'checkbox', name: 'environment', value: environment.name, checked: true }),
        element('span', {}, [
          element('strong', { textContent: environment.name }),
          element('span', { className: 'mono', textContent: environment.services.join('   ') }),
          element('span', { className: 'folder', textContent: environment.folder }),
        ]),
      ]),
      removal(environment.folder),
    ])));
  chosen();
  show('setup');
}

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
  $('added').textContent = 'Reading the compose file…';
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

$('choose').addEventListener('submit', async (event) => {
  event.preventDefault();
  $('error').textContent = '';
  $('share').disabled = true;
  $('share').textContent = 'Starting…';
  try {
    await invoke('share', {
      environments: chosen(),
      minutes: Number($('minutes').value),
      guests: Number($('limit').value),
    });
  } catch (error) {
    $('error').textContent = String(error);
  }
  $('share').textContent = 'Share';
  chosen();
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
    ? 'The link and the QR code work from any network. The code alone is for someone on this network.'
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

listen('session', (event) => render(event.payload));
listen('ended', (event) => {
  forget();
  setup(event.payload);
});

setup();
