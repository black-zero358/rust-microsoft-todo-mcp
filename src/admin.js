'use strict';
const $ = id => document.getElementById(id);
let loginId = null;
let nextPoll = 0;
async function api(path, method = 'GET') {
  const response = await fetch('/admin/api/' + path, {method, headers: {Authorization: 'Bearer ' + $('token').value}, credentials: 'omit', cache: 'no-store'});
  let body; try { body = await response.json(); } catch { body = {error: 'Request failed (' + response.status + ')'}; }
  $('output').textContent = JSON.stringify(body, null, 2);
  if (!response.ok) throw new Error('Request failed');
  return body;
}
function action(label, callback) {const button = document.createElement('button'); button.textContent = label; button.onclick = async () => {button.disabled = true; try {await callback();} catch {} finally {button.disabled = false;}}; return button;}
async function load() {
  const result = await api('accounts'); $('accounts').replaceChildren();
  for (const row of result.accounts) {
    const a = row.account; const item = document.createElement('p');
    const title = document.createElement('span'); title.textContent = a.display_name + (a.is_default ? ' (default)' : '') + ' · ' + row.sync.state + ' ';
    const id = encodeURIComponent(a.id); item.append(title,
      action('Synchronize', async () => {await api('accounts/' + id + '/refresh', 'POST'); await load();}),
      action('Set default', async () => {await api('accounts/' + id + '/default', 'POST'); await load();}),
      action('Disconnect and erase local data', async () => {if (confirm('Disconnect this account and remove its local credentials and cached tasks? Microsoft tasks stay unchanged.')) {await api('accounts/' + id, 'DELETE'); await load();}}));
    $('accounts').append(item);
  }
}
$('connect').onclick = () => load().catch(() => {});
$('add').onclick = async () => {try {const result = await api('login', 'POST'); loginId = result.login_id; nextPoll = Date.now() + result.interval * 1000; $('login').textContent = 'Open ' + result.verification_uri + ' and enter code ' + result.user_code + '. After signing in, choose Check sign-in.'; $('poll').hidden = false;} catch {}};
$('poll').onclick = async () => {if (!loginId || Date.now() < nextPoll) return; nextPoll = Date.now() + 5000; try {const result = await api('login/' + encodeURIComponent(loginId), 'POST'); if (result.status === 'connected') {loginId = null; $('poll').hidden = true; $('login').textContent = 'Account connected. Synchronize it to populate the cache.'; await load();}} catch {}};
