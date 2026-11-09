const act = action => window.__TAURI_INTERNALS__.invoke('window_action', {action}).catch(console.error);
document.querySelector('#cancel').addEventListener('click', () => act('cancel_exit'));
document.querySelector('#exit').addEventListener('click', () => act('confirm_exit'));
document.addEventListener('keydown', event => {
  if(event.key === 'Escape' || (event.altKey && event.key === 'F4')) {
    event.preventDefault(); act('cancel_exit');
  }
  if(event.key === 'Tab') {
    event.preventDefault();
    (document.activeElement.id === 'cancel' ? document.querySelector('#exit') : document.querySelector('#cancel')).focus();
  }
});
window.addEventListener('DOMContentLoaded', () => document.querySelector('#cancel').focus());
act('dialog_ready');
