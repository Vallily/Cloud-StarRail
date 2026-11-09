let fullscreen = false;
let revealed = false;
let hideTimer;
window.setFullscreen = (full, open = false) => {
  fullscreen = full; revealed = full && open;
  document.body.classList.toggle("revealed", revealed);
  document.body.classList.toggle('full', full);
  const button = document.querySelector('[data-action="toggle"]');
  button.title = full ? '窗口化' : '无边框全屏';
  button.setAttribute('aria-label', button.title);
  document.querySelector('#mode').innerHTML = full
    ? '<path d="M7 4h9v9M4 7h9v9H4z"/>'
    : '<rect x="4" y="4" width="12" height="12" rx="1"/>';
};
document.querySelectorAll('button').forEach(button => button.addEventListener('click', event => {
  if (event.detail > 0) button.blur();
  window.__TAURI_INTERNALS__.invoke('window_action', {action:button.dataset.action}).catch(console.error);
}));
// Ask the host to refresh mode after this view has finished loading.
window.__TAURI_INTERNALS__.invoke('window_action', {action:'sync'}).catch(console.error);
// Native code reveals the hidden view only inside the top/right 10px edge band.
// Once shown, the entire original 176x64 rectangle remains interactive.
function setRevealed(open) {
  if (!fullscreen || revealed === open) return;
  revealed = open;
  document.body.classList.toggle('revealed', open);
  window.__TAURI_INTERNALS__.invoke('window_action', {
    action: open ? 'controls_open' : 'controls_close'
  }).catch(console.error);
}
document.addEventListener('pointermove', () => clearTimeout(hideTimer));
document.documentElement.addEventListener('pointerleave', () => {
  clearTimeout(hideTimer);
  hideTimer = setTimeout(() => {
    if (!document.documentElement.matches(':hover')) setRevealed(false);
  }, 240);
});
document.addEventListener('focusin', () => { clearTimeout(hideTimer); setRevealed(true); });