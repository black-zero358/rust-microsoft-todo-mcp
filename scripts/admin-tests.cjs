'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(require('node:path').join(__dirname, '../src/admin.js'), 'utf8');
function element() {return {value: '', disabled: false, hidden: false, textContent: '', children: [], events: {}, append(...items) {this.children.push(...items);}, replaceChildren(...items) {this.children = items;}, addEventListener(name, handler) {this.events[name] = handler;}};}
function harness() {
  const elements = Object.fromEntries(['token','connect','add','accounts','login','poll','output'].map(id => [id, element()]));
  elements.token.value = 'synthetic-admin-token';
  const pending = []; let now = 0;
  const context = vm.createContext({document: {getElementById: id => elements[id], createElement: element}, confirm: () => false, Date: {now: () => now}, fetch: (...args) => new Promise((resolve, reject) => pending.push({args, resolve, reject}))});
  vm.runInContext(source, context);
  return {elements, pending, context, time(value) {now = value;}};
}
const respond = (call, body, status = 200) => call.resolve({ok: status >= 200 && status < 300, status, json: async () => body});
const account = name => ({accounts: [{account: {id: name, display_name: name}, sync: {state: 'ready'}}]});
test('network failures remain visible and buttons are re-enabled', async () => {
  const h = harness(); const work = h.elements.connect.onclick(); h.pending[0].reject(new Error('offline')); await work;
  assert.match(h.elements.output.textContent, /Check the connection/); assert.equal(h.elements.connect.disabled, false);
});
test('repeated add clicks create only one login request', async () => {
  const h = harness(); const first = h.elements.add.onclick(); const second = h.elements.add.onclick();
  assert.equal(h.pending.length, 1); respond(h.pending[0], {login_id: 'id', interval: 5, user_code: 'code', verification_uri: 'https://microsoft.com/devicelogin'}); await Promise.all([first, second]);
  assert.equal(h.elements.add.disabled, false); assert.equal(h.elements.poll.hidden, false);
});
test('late account list response never overwrites a newer view', async () => {
  const h = harness(); const first = vm.runInContext('load()', h.context); const second = vm.runInContext('load()', h.context);
  respond(h.pending[1], account('newer')); await second; respond(h.pending[0], account('older')); await first;
  assert.match(h.elements.accounts.children[0].children[0].textContent, /newer/);
});
test('slow_down polling intervals are followed by the management page', async () => {
  const h = harness(); const add = h.elements.add.onclick(); respond(h.pending[0], {login_id: 'id', interval: 5, user_code: 'code', verification_uri: 'https://microsoft.com/devicelogin'}); await add;
  h.time(5000); const poll = h.elements.poll.onclick(); respond(h.pending[1], {status: 'pending', interval: 120}); await poll;
  h.time(6000); await h.elements.poll.onclick(); assert.equal(h.pending.length, 2); assert.match(h.elements.output.textContent, /119 seconds/);
});
test('changing administrator token discards pending private responses', async () => {
  const h = harness(); const work = h.elements.connect.onclick(); h.elements.token.value = 'different-token'; h.elements.token.events.input();
  respond(h.pending[0], account('old-private-name')); await work;
  assert.equal(h.elements.accounts.children.length, 0); assert.equal(h.elements.output.textContent, '');
});
test('expired login resets the sign-in controls', async () => {
  const h = harness(); const add = h.elements.add.onclick(); respond(h.pending[0], {login_id: 'id', interval: 5, user_code: 'code', verification_uri: 'https://microsoft.com/devicelogin'}); await add;
  h.time(5000); const poll = h.elements.poll.onclick(); respond(h.pending[1], {error: {code: 'login_expired'}}, 400); await poll;
  assert.equal(h.elements.poll.hidden, true); assert.match(h.elements.login.textContent, /Start a new/);
});
