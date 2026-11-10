// System GameInput ABI from the Windows SDK. No redistributable or Xbox hooks.
#include <windows.h>
#include <GameInput.h>
#include <atomic>
struct ShareInput { HMODULE module{}; IGameInput* input{}; GameInputCallbackToken token{}; HWND window{}; std::atomic<bool> pending{false}; };
static void CALLBACK on_share(GameInputCallbackToken, void* context, IGameInputDevice* device, uint64_t, GameInputSystemButtons current, GameInputSystemButtons previous) {
    auto* self=static_cast<ShareInput*>(context);
    if ((current & GameInputSystemButtonShare) && !(previous & GameInputSystemButtonShare) && device->GetDeviceInfo()->vendorId==0x045e && GetForegroundWindow()==self->window) self->pending.store(true);
}
extern "C" void* xbox_share_open(void* hwnd) {
    auto* self=new ShareInput; self->window=static_cast<HWND>(hwnd);
    self->module=LoadLibraryExW(L"GameInput.dll",nullptr,LOAD_LIBRARY_SEARCH_SYSTEM32);
    if(self->module){auto create=reinterpret_cast<HRESULT(WINAPI*)(IGameInput**)>(GetProcAddress(self->module,"GameInputCreate"));
        if(create && SUCCEEDED(create(&self->input)) && SUCCEEDED(self->input->RegisterSystemButtonCallback(nullptr,GameInputSystemButtonShare,self,on_share,&self->token)))return self;}
    if(self->input)self->input->Release(); if(self->module)FreeLibrary(self->module); delete self;return nullptr;
}
extern "C" bool xbox_share_take(void* ptr){return ptr && static_cast<ShareInput*>(ptr)->pending.exchange(false);}
extern "C" void xbox_share_close(void* ptr){if(!ptr)return;auto* self=static_cast<ShareInput*>(ptr);self->input->StopCallback(self->token);self->input->UnregisterCallback(self->token,UINT64_MAX);self->input->Release();FreeLibrary(self->module);delete self;}
