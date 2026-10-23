// ---------- boot ----------
if (GUEST) bootGuest();
else loadState().then(route).catch(e => toast(e.message, { error: true }));

