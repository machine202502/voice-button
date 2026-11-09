// Voice Button API demo: captures this page's microphone and streams PCM16 to
// the app, which transcribes it. Nothing is bundled into the app itself.

import { encodePcm16 } from './pcm.js';

const $ = (id) => document.getElementById(id);
const STORAGE_API = 'voice-button-api';
const STORAGE_MODEL = 'voice-button-api-model';
const DEFAULT_API = 'http://127.0.0.1:8931';
const CHUNK_MS = 250;

const WORKLET = `
class PcmTap extends AudioWorkletProcessor {
  process(inputs) {
    const input = inputs[0];
    if (input && input[0] && input[0].length) this.port.postMessage(input[0].slice(0));
    return true;
  }
}
registerProcessor('pcm-tap', PcmTap);
`;

let lastStatus = null;
let capturing = false;
let session = null;
let tracked = null;
let resultShown = null;
let queue = [];
let sendTask = null;
let audio = null;
let lastReceived = 0;
let busy = false;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function apiBase() {
  return ($('api').value || DEFAULT_API).trim().replace(/\/+$/, '');
}

function setError(text) {
  const box = $('err');
  if (!text) {
    box.style.display = 'none';
    box.textContent = '';
    return;
  }
  box.style.display = 'block';
  box.textContent = text;
}

async function api(path, options) {
  const response = await fetch(apiBase() + path, Object.assign({ cache: 'no-store' }, options || {}));
  const raw = await response.text();
  let data = null;
  try {
    data = raw ? JSON.parse(raw) : null;
  } catch {
    data = { raw };
  }
  if (!response.ok) {
    const message = (data && data.error) || `HTTP ${response.status}`;
    const error = new Error(message);
    error.status = response.status;
    throw error;
  }
  return data;
}

function connected(ok) {
  $('dot').className = ok ? 'dot ok' : 'dot bad';
  $('conn').textContent = ok ? 'connected' : 'no connection';
}

async function loadModels() {
  try {
    const models = await api('/api/models');
    const select = $('model');
    const previous = localStorage.getItem(STORAGE_MODEL) || '';
    select.innerHTML = '<option value="">как в настройках приложения</option>';
    for (const item of models) {
      const option = document.createElement('option');
      option.value = item.id;
      option.textContent = item.id + (item.ready ? '' : ' — не скачана');
      option.disabled = !item.ready;
      select.appendChild(option);
    }
    select.value = models.some((m) => m.id === previous && m.ready) ? previous : '';
  } catch (e) {
    setError(`Не удалось получить список моделей: ${e.message}`);
  }
}

function describe(status) {
  const owner = status.owner;
  if (status.state === 'recording') {
    if (owner === 'server') {
      return { state: 'recording', big: 'Запись (сервер)', sub: 'Идёт чья-то API-сессия.' };
    }
    return { state: 'busy', big: 'Приложение занято', sub: 'Сейчас диктует пользователь в приложении — попробуйте через несколько секунд.' };
  }
  if (status.state === 'transcribing') {
    return { state: 'busy', big: 'Распознавание…', sub: 'Секунду — текст придёт на страницу.' };
  }
  if (status.state === 'nomodel') {
    return { state: 'idle', big: 'Нет модели', sub: 'В приложении не выбрана скачанная модель.' };
  }
  return { state: 'idle', big: 'idle', sub: 'Готов к записи.' };
}

function render(status) {
  lastStatus = status;
  connected(true);
  const view = describe(status);
  const mic = $('mic');
  if (busy) {
    mic.dataset.state = 'busy';
  } else if (capturing) {
    mic.dataset.state = 'recording';
  } else {
    mic.dataset.state = view.state;
  }
  if (capturing) {
    $('state').textContent = 'Отправка звука';
    const seconds = (lastReceived / 1000).toFixed(1);
    $('sub').textContent = `Микрофон страницы → приложение · отправлено ${seconds} с`;
  } else {
    $('state').textContent = view.big;
    $('sub').textContent = view.sub;
  }
  $('live').textContent = status.liveText || '';
  $('live-meta').textContent = status.liveText ? '● live' : '';
  // The session can be ended from the app (the user has priority); stop
  // sending instead of posting audio into a dead session.
  if (capturing && (status.sessionId === null || status.sessionId === undefined)) {
    endLocally('Сессию завершили в приложении — распознанный текст сохранён в истории.');
  }
  if (status.sessionId !== null && status.sessionId !== undefined) {
    tracked = status.sessionId;
  } else if (tracked !== null) {
    tracked = null;
    loadResult();
  }
}

async function loadResult() {
  try {
    const data = await api('/api/record/text');
    if (data.sessionId === null || data.sessionId === undefined) return;
    if (resultShown === data.sessionId) return;
    resultShown = data.sessionId;
    $('result').textContent = data.text || '(пусто)';
    const who = data.stoppedBy === 'user' ? 'остановлено в приложении' : 'остановлено со страницы';
    $('result-meta').textContent = `${data.at || ''} · ${who}`;
    if (data.error) setError(`Распознавание не удалось: ${data.error}`);
  } catch (e) {
    setError(`Не удалось получить текст: ${e.message}`);
  }
}

let pollingLock = false;
async function poll() {
  if (pollingLock) return;
  pollingLock = true;
  try {
    render(await api('/api/status'));
  } catch {
    connected(false);
    setError('Сервер недоступен: включите API во вкладке API и проверьте адрес.');
  } finally {
    pollingLock = false;
  }
}

async function startCapture() {
  if (busy || capturing) return;
  setError('');
  if (lastStatus && lastStatus.state === 'recording' && lastStatus.owner !== 'server') {
    setError('В приложении уже идёт запись пользователя — дождитесь её окончания.');
    return;
  }
  busy = true;
  renderBusy();
  let media = null;
  let ctx = null;
  let graph = null;
  try {
    media = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    ctx = new AudioContext({ sampleRate: 16000 });
    await ctx.resume();
    const moduleUrl = URL.createObjectURL(new Blob([WORKLET], { type: 'application/javascript' }));
    await ctx.audioWorklet.addModule(moduleUrl);
    URL.revokeObjectURL(moduleUrl);
    const source = ctx.createMediaStreamSource(media);
    const node = new AudioWorkletNode(ctx, 'pcm-tap');
    const mute = ctx.createGain();
    mute.gain.value = 0;
    source.connect(node);
    node.connect(mute);
    mute.connect(ctx.destination);
    graph = { ctx, media, node, source, mute };
    queue = [];
    node.port.onmessage = (event) => queue.push(event.data);

    const model = $('model').value;
    const started = await api('/api/record/start', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(model ? { model, sampleRate: ctx.sampleRate } : { sampleRate: ctx.sampleRate }),
    });
    audio = graph;
    session = started.sessionId;
    tracked = session;
    resultShown = null;
    lastReceived = 0;
    $('result').textContent = '';
    $('result-meta').textContent = '';
    capturing = true;
    sendTask = sendLoop();
  } catch (e) {
    releaseAudio(audio || graph || (media && ctx ? { ctx, media } : null));
    setError(describeCaptureError(e));
  } finally {
    busy = false;
    renderBusy();
    poll();
  }
}

function releaseAudio(graph) {
  if (!graph) return;
  try {
    if (graph.node) {
      graph.node.port.onmessage = null;
      graph.node.disconnect();
    }
    if (graph.source) graph.source.disconnect();
    if (graph.mute) graph.mute.disconnect();
  } catch {
    // already torn down
  }
  if (graph.media) graph.media.getTracks().forEach((track) => track.stop());
  if (graph.ctx) graph.ctx.close().catch(() => {});
}

function describeCaptureError(e) {
  if (e && e.name === 'NotAllowedError') {
    return 'Браузер не дал доступ к микрофону. Разрешите его для этой страницы.';
  }
  if (e && e.name === 'NotFoundError') return 'Микрофон не найден.';
  if (e && e.name === 'SecurityError') {
    return 'Микрофон доступен только на localhost или по HTTPS. Откройте страницу с 127.0.0.1/localhost.';
  }
  return e && e.message ? e.message : String(e);
}

function renderBusy() {
  $('mic').disabled = busy;
  if (busy) $('mic').dataset.state = 'busy';
}

async function sendLoop() {
  while (capturing) {
    await sleep(CHUNK_MS);
    await flush();
  }
  await flush();
}

async function flush() {
  if (!queue.length || !session) return;
  const frames = queue;
  queue = [];
  const pcm = encodePcm16(frames);
  if (!pcm.length) return;
  try {
    const reply = await api(`/api/record/audio?session=${session}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/octet-stream' },
      body: pcm.buffer,
    });
    lastReceived = reply.receivedMs || lastReceived;
  } catch (e) {
    if (e.status === 409) {
      endLocally('Сессию остановили в приложении — текст сохранён в истории.');
    } else {
      setError(`Не удалось отправить звук: ${e.message}`);
    }
  }
}

async function stopCapture() {
  if (!capturing || busy) return;
  busy = true;
  renderBusy();
  try {
    capturing = false;
    const graph = audio;
    audio = null;
    releaseAudio(graph);
    if (sendTask) {
      await sendTask;
      sendTask = null;
    }
    await api(`/api/record/stop?session=${session}`, { method: 'POST' });
  } catch (e) {
    setError(`Не удалось остановить сессию: ${e.message}`);
  } finally {
    busy = false;
    renderBusy();
    poll();
  }
}

/**
 * Release the microphone without talking to the API: the session is already
 * over (stopped in the app, or the server refused the audio).
 */
function endLocally(message) {
  capturing = false;
  const graph = audio;
  audio = null;
  releaseAudio(graph);
  session = null;
  setError(message);
}

$('mic').addEventListener('click', () => {
  if (capturing) stopCapture();
  else startCapture();
});

$('reload').addEventListener('click', () => {
  loadModels();
  poll();
});

$('api').addEventListener('change', () => {
  const value = apiBase();
  $('api').value = value;
  localStorage.setItem(STORAGE_API, value);
  setError('');
  loadModels();
  poll();
});

$('model').addEventListener('change', () => {
  localStorage.setItem(STORAGE_MODEL, $('model').value);
});

$('api').value = localStorage.getItem(STORAGE_API) || DEFAULT_API;
loadModels();
poll();
setInterval(poll, 500);
