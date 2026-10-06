// The other services of the showcase, on the hostname this page was opened
// with: localhost on the machine that runs it, showcase.test for a guest.
// The ports are those of .env.
const SERVICES = { web: location.port, api: 8711, docs: 8712 };

const address = (service) => `${location.protocol}//${location.hostname}:${SERVICES[service]}`;

function report(service, ok, answer) {
  const row = document.getElementById(service);
  row.className = ok ? 'ok' : 'failed';
  row.cells[2].textContent = answer;
}

document.getElementById('here').textContent = location.host;
for (const service of Object.keys(SERVICES)) {
  document.getElementById(service).cells[1].firstChild.textContent = address(service);
}

fetch(`${address('api')}/hello`)
  .then((response) => response.json())
  .then((answer) => report('api', true, `asked as ${answer.host}`))
  .catch(() => report('api', false, 'not reachable'));

// Another origin without CORS: the answer cannot be read, only that it came.
fetch(address('docs'), { mode: 'no-cors' })
  .then(() => report('docs', true, 'answered'))
  .catch(() => report('docs', false, 'not reachable'));
