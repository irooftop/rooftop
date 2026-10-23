// ---------- voice messages (record → 16 kHz mono WAV → upload, then the server transcribes) ----------
const REC = { recorder: null, stream: null, chunks: [], start: 0, timer: null, send: false };
const fmtSecs = s => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
async function startRecording() {
  if (REC.recorder) return;
  try {
    REC.stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } });
  } catch (e) {
    return toast(e.name === "NotAllowedError" ? "Microphone access was denied." : `Microphone unavailable: ${e.message}`, { error: true });
  }
  REC.chunks = []; REC.send = false;
  REC.recorder = new MediaRecorder(REC.stream);
  REC.recorder.ondataavailable = e => { if (e.data.size) REC.chunks.push(e.data); };
  REC.recorder.onstop = finishRecording;
  REC.recorder.start(250);
  REC.start = Date.now();
  $(".compose-row").classList.add("hidden"); $("#recBar").classList.remove("hidden");
  $("#recHint").textContent = "Recording…"; $("#recTime").textContent = "0:00";
  REC.timer = setInterval(() => {
    const s = (Date.now() - REC.start) / 1000;
    $("#recTime").textContent = fmtSecs(s);
    if (s >= 300) stopRecording(true); // 5 minutes max
  }, 250);
  $("#recSend").focus();
}
function stopRecording(send) {
  if (!REC.recorder) return;
  REC.send = send;
  clearInterval(REC.timer);
  if (REC.recorder.state !== "inactive") REC.recorder.stop();
}
async function finishRecording() {
  const duration = (Date.now() - REC.start) / 1000;
  REC.stream?.getTracks().forEach(t => t.stop());
  const blob = new Blob(REC.chunks, { type: REC.recorder.mimeType || "audio/webm" });
  REC.recorder = null; REC.stream = null;
  $("#recBar").classList.add("hidden"); $(".compose-row").classList.remove("hidden");
  if (!REC.send) return;
  if (duration < 0.6) return toast("Hold on a bit longer: the message was too short.");
  try {
    const wav = await toWav(blob);
    const fd = new FormData();
    fd.append("file", wav, "voice.wav");
    fd.append("duration", String(Math.round(duration)));
    if (S.reply) fd.append("reply_to", String(S.reply.id));
    S.reply = null; renderReply();
    $("#micBtn").disabled = true;
    const r = await api(chatPath("voice"), { method: "POST", body: fd });
    upsertMessages(r.messages); renderMessages(); scheduleRespond();
  } catch (e) { toast(`Voice message not sent: ${e.message}`, { error: true }); }
  finally { $("#micBtn").disabled = false; }
}
// Any browser recording (webm/opus, mp4/aac, ogg) → 16 kHz mono PCM WAV: small, plays everywhere, transcribable.
async function toWav(blob) {
  const AC = window.AudioContext || window.webkitAudioContext;
  const ctx = new AC();
  let buf;
  try { buf = await ctx.decodeAudioData(await blob.arrayBuffer()); } finally { ctx.close(); }
  const rate = 16000, frames = Math.max(1, Math.ceil(buf.duration * rate));
  const off = new OfflineAudioContext(1, frames, rate);
  const src = off.createBufferSource(); src.buffer = buf; src.connect(off.destination); src.start();
  const pcm = (await off.startRendering()).getChannelData(0);
  const out = new DataView(new ArrayBuffer(44 + pcm.length * 2));
  const str = (o, t) => { for (let i = 0; i < t.length; i++) out.setUint8(o + i, t.charCodeAt(i)); };
  str(0, "RIFF"); out.setUint32(4, 36 + pcm.length * 2, true); str(8, "WAVEfmt ");
  out.setUint32(16, 16, true); out.setUint16(20, 1, true); out.setUint16(22, 1, true);
  out.setUint32(24, rate, true); out.setUint32(28, rate * 2, true); out.setUint16(32, 2, true); out.setUint16(34, 16, true);
  str(36, "data"); out.setUint32(40, pcm.length * 2, true);
  for (let i = 0; i < pcm.length; i++) out.setInt16(44 + i * 2, Math.max(-1, Math.min(1, pcm[i])) * 0x7fff, true);
  return new Blob([out], { type: "audio/wav" });
}
$("#micBtn").onclick = () => startRecording();
$("#recSend").onclick = () => stopRecording(true);
$("#recCancel").onclick = () => stopRecording(false);
document.addEventListener("keydown", e => { if (e.key === "Escape" && REC.recorder) stopRecording(false); });
autosize();

