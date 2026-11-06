'use strict';
const $ = id => document.getElementById(id);
let loginId = null;
let nextPoll = 0;
let outputVersion = 0;
let loadVersion = 0;
let loginVersion = 0;
async function api(path, method = 'GET') {
  const token = $('token').value;
  const version = ++outputVersion;
  let response;
  try {
    response = await fetch('/admin/api/' + path, {method, headers: {Authorization: 'Bearer ' + token}, credentials: 'omit', cache: 'no-store'});
  } catch {
    if (token === $('token').value && version === outputVersion) $('output').textContent = 'Request failed. Check the connection and try again.';
    throw new Error('Connection failed');
  }
  let body;
  try { body = await response.json(); } catch { body = {error: 'Request failed (' + response.status + ')'}; }
  if (token !== $('token').value) throw new Error('Administrator token changed');
  if (version === outputVersion) $('output').textContent = JSON.stringify(body, null, 2);
  if (!response.ok) {const error = new Error('Request failed'); error.response = body; throw error;}
  return body;
}
function withButton(button, callback) {
  return async () => {
    if (button.disabled) return;
    button.disabled = true;
    try {await callback();} catch {} finally {button.disabled = false;}
  };
}
function action(label, callback) {
  const button = document.createElement('button'); button.textContent = label;
  button.onclick = withButton(button, callback); return button;
}
async function load() {
  const version = ++loadVersion;
  const result = await api('accounts');
  if (version !== loadVersion) return;
  $('accounts').replaceChildren();
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
$('connect').onclick = withButton($('connect'), load);
$('add').onclick = withButton($('add'), async () => {
  const version = ++loginVersion;
  loginId = null; $('poll').hidden = true;
  const result = await api('login', 'POST');
  if (version !== loginVersion) return;
  loginId = result.login_id; nextPoll = Date.now() + result.interval * 1000;
  $('login').textContent = 'Open ' + result.verification_uri + ' and enter code ' + result.user_code + '. After signing in, choose Check sign-in.';
  $('poll').hidden = false;
});
$('poll').onclick = withButton($('poll'), async () => {
  if (!loginId) return;
  if (Date.now() < nextPoll) {$('output').textContent = 'Wait ' + Math.ceil((nextPoll - Date.now()) / 1000) + ' seconds before checking again.'; return;}
  const id = loginId;
  try {
    const result = await api('login/' + encodeURIComponent(id), 'POST');
    if (loginId !== id) return;
    nextPoll = Date.now() + (result.interval || 5) * 1000;
    if (result.status === 'connected') {
      loginId = null; $('poll').hidden = true;
      $('login').textContent = 'Account connected. Synchronize it to populate the cache.';
      await load();
    }
  } catch (error) {
    if (loginId === id && ['login_expired', 'authorization_failed'].includes(error.response?.error?.code)) {
      loginId = null; $('poll').hidden = true; $('login').textContent = 'Sign-in ended. Start a new Microsoft account login.';
    }
  }
});
$('token').addEventListener('input', () => {
  outputVersion++; loadVersion++; loginVersion++; loginId = null; nextPoll = 0;
  $('accounts').replaceChildren(); $('output').textContent = ''; $('login').textContent = ''; $('poll').hidden = true;
});
