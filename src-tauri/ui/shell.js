import {icons} from './icons.js';
const invoke = (action) => window.__TAURI_INTERNALS__.invoke('window_action', {action}).catch(e=>window.showNotice(String(e)));
document.querySelector('header').addEventListener('mousedown', e => { if(!e.target.closest('button') && e.button === 0 && e.detail === 1) invoke('drag'); });
document.querySelector('header').addEventListener('dblclick', e => { if(!e.target.closest('button')) invoke('toggle'); });
const record = document.querySelector('#record');
record.addEventListener('click', () => invoke('record_toggle'));
document.querySelector('#shot').addEventListener('click',()=>invoke('screenshot'));
const settingsButton=document.querySelector('#settings');
settingsButton.addEventListener('pointerdown',e=>{if(e.button===0)invoke('settings');});
settingsButton.addEventListener('click',e=>{if(e.detail===0)invoke('settings');});
function syncSettingsAnchor(){const r=settingsButton.getBoundingClientRect();window.__TAURI_INTERNALS__.invoke('media_settings',{action:'anchor',anchor:[r.left,r.top,r.right,r.bottom]}).catch(console.error);}
window.addEventListener('resize',syncSettingsAnchor);
new ResizeObserver(syncSettingsAnchor).observe(settingsButton);
syncSettingsAnchor();
window.setRecordingStatus = (text, busy) => {
  if(record.dataset.busy!==String(busy))record.innerHTML=icons[busy?'square':'video'];
  record.dataset.busy = String(busy); record.setAttribute('aria-label',busy?'停止录屏':'开始录屏');
  const status=document.querySelector('#record-status');status.textContent=text;status.title=text;
};
window.setShellFullscreen = full => { document.querySelector('header').hidden=full; };
window.showNotice = text => {const status=document.querySelector('#record-status');status.textContent=text;status.title=text;};
invoke('sync');
