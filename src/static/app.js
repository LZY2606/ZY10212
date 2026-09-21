"use strict";
let S = null;
let selectedScan = 1;
let selectedCand = null;
let pendingSurfaceSample = null;
let trackPick = new Set();

const $ = (id) => document.getElementById(id);
const fmt = (v, d = 2) => (v === null || v === undefined || Number.isNaN(v)) ? "—" : Number(v).toFixed(d);

async function api(path, opts) {
  const res = await fetch(path, opts);
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || ("HTTP " + res.status));
  return data;
}

async function postEvent(event, note) {
  return api("/api/state", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(note ? { ...event, note } : event),
  });
}

function curScan() { return S.scans.find((s) => s.id === selectedScan); }

function kindLabel(k) {
  return { single: "单峰", composite: "不可分辨复合", saturated: "饱和平台", split: "拆分峰" }[k] || k;
}
function verdictLabel(v) {
  return { scatter: "真实散射", surface: "表面回波", bottom: "底面回波", overlap: "重叠回波" }[v] || "未裁决";
}
function gateLabel(g) {
  return { surface: "表面门", inspection: "检测门", bottom: "底面门", "": "无闸门" }[g] || g;
}

function drawMap() {
  const cv = $("mapCanvas"), ctx = cv.getContext("2d");
  ctx.clearRect(0, 0, cv.width, cv.height);
  const xs = S.scans.map((s) => s.x_mm), ys = S.scans.map((s) => s.y_mm);
  const minX = Math.min(...xs), maxX = Math.max(...xs), minY = Math.min(...ys), maxY = Math.max(...ys);
  const P = 26;
  const px = (x) => P + ((x - minX) / Math.max(1, maxX - minX)) * (cv.width - 2 * P);
  const py = (y) => cv.height - P - ((y - minY) / Math.max(1, maxY - minY)) * (cv.height - 2 * P);
  // adjacency links
  ctx.strokeStyle = "#2a3550"; ctx.lineWidth = 1;
  for (const a of S.scans) for (const b of S.scans) {
    if (b.id <= a.id) continue;
    const d = Math.hypot(a.x_mm - b.x_mm, a.y_mm - b.y_mm);
    if (d <= 5.0001) { ctx.beginPath(); ctx.moveTo(px(a.x_mm), py(a.y_mm)); ctx.lineTo(px(b.x_mm), py(b.y_mm)); ctx.stroke(); }
  }
  for (const s of S.scans) {
    const x = px(s.x_mm), y = py(s.y_mm);
    const hasTrack = s.candidates.some((c) => c.track_id !== null);
    ctx.beginPath(); ctx.arc(x, y, s.id === selectedScan ? 8 : 6, 0, Math.PI * 2);
    ctx.fillStyle = s.id === selectedScan ? "#7aa2ff" : hasTrack ? "#34d399" : "#3a4a72";
    ctx.fill();
    ctx.fillStyle = "#8b97b3"; ctx.font = "10px sans-serif"; ctx.textAlign = "center";
    ctx.fillText("#" + s.id, x, y - 12);
  }
  $("mapInfo").textContent = `位置 ${selectedScan}: x=${fmt(curScan().x_mm, 1)}mm y=${fmt(curScan().y_mm, 1)}mm`;
  cv.onclick = (e) => {
    const rect = cv.getBoundingClientRect();
    const mx = (e.clientX - rect.left) * cv.width / rect.width;
    const my = (e.clientY - rect.top) * cv.height / rect.height;
    let best = null, bd = 1e9;
    for (const s of S.scans) {
      const d = Math.hypot(mx - px(s.x_mm), my - py(s.y_mm));
      if (d < bd) { bd = d; best = s; }
    }
    if (best && bd < 20) { selectedScan = best.id; selectedCand = null; render(); }
  };
}

function xOfSample(n, w) {
  return 40 + (n / S.n) * (w - 60);
}
function sampleAtX(x, w) {
  return Math.round(((x - 40) / (w - 60)) * S.n);
}
// Display arrays are downsampled by scan.view_stride; map index -> true sample.
function viewN(scan) { return scan.raw_view.length; }

function drawWave() {
  const cv = $("wave"); const w = cv.width, h = cv.height;
  const ctx = cv.getContext("2d");
  ctx.clearRect(0, 0, w, h);
  const scan = curScan();
  const rawMax = S.adc_max;
  const yOfRaw = (v) => h / 2 - (v / rawMax) * (h / 2 - 24) * 0.55;
  const envMax = Math.max(...scan.envelope_view.slice(0, 500), 1);
  const yOfEnv = (v) => h / 2 - (v / envMax) * (h / 2 - 24) * 0.95;

  // gates
  const gateColor = { surface: "rgba(45,212,191,.18)", inspection: "rgba(122,162,255,.10)", bottom: "rgba(251,191,36,.14)" };
  for (const g of S.gates) {
    ctx.fillStyle = gateColor[g.kind] || "rgba(255,255,255,.05)";
    ctx.fillRect(xOfSample(g.lo, w), 10, xOfSample(g.hi, w) - xOfSample(g.lo, w), h - 40);
    ctx.strokeStyle = "#2dd4bf"; ctx.setLineDash([4, 3]); ctx.lineWidth = 1;
    ctx.beginPath(); ctx.moveTo(xOfSample(g.lo, w), 10); ctx.lineTo(xOfSample(g.lo, w), h - 30); ctx.stroke();
    ctx.beginPath(); ctx.moveTo(xOfSample(g.hi, w), 10); ctx.lineTo(xOfSample(g.hi, w), h - 30); ctx.stroke();
    ctx.setLineDash([]);
    ctx.fillStyle = "#8b97b3"; ctx.font = "10px sans-serif"; ctx.textAlign = "center";
    ctx.fillText(gateLabel(g.kind), (xOfSample(g.lo, w) + xOfSample(g.hi, w)) / 2, 22);
  }

  // axis
  ctx.strokeStyle = "#3a4560"; ctx.beginPath(); ctx.moveTo(40, h / 2); ctx.lineTo(w - 20, h / 2); ctx.stroke();
  for (let us = 0; us <= 20; us += 2) {
    const n = us * S.fs_mhz; if (n > S.n) break;
    const x = xOfSample(n, w);
    ctx.fillStyle = "#5b688f"; ctx.font = "9px sans-serif"; ctx.textAlign = "center";
    ctx.fillText(us + "μs", x, h - 14);
  }

  // raw waveform (downsampled view; full samples live in fixture/export)
  ctx.strokeStyle = "#7aa2ff"; ctx.lineWidth = 1; ctx.beginPath();
  for (let k = 0; k < viewN(scan); k++) {
    const n = k * scan.view_stride;
    const x = xOfSample(n, w), y = yOfRaw(scan.raw_view[k]);
    k === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
  }
  ctx.stroke();
  // envelope
  ctx.strokeStyle = "#ffd166"; ctx.lineWidth = 1.4; ctx.beginPath();
  for (let k = 0; k < viewN(scan); k++) {
    const n = k * scan.view_stride;
    const x = xOfSample(n, w), y = yOfEnv(scan.envelope_view[k]);
    k === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
  }
  ctx.stroke();
  // threshold line
  const thrNorm = S.threshold_mode === "snr" ? S.threshold_snr * scan.noise_floor : S.threshold_amp / S.adc_scale;
  ctx.strokeStyle = "#ff8080"; ctx.setLineDash([6, 4]); ctx.lineWidth = 1;
  const yt = yOfEnv(thrNorm);
  ctx.beginPath(); ctx.moveTo(40, yt); ctx.lineTo(w - 20, yt); ctx.stroke(); ctx.setLineDash([]);
  ctx.fillStyle = "#ff8080"; ctx.textAlign = "left"; ctx.fillText("阈值", w - 60, yt - 4);

  // surface line
  const sx = xOfSample(scan.surface_sample, w);
  ctx.strokeStyle = "#34d399"; ctx.lineWidth = 1.5;
  ctx.beginPath(); ctx.moveTo(sx, 10); ctx.lineTo(sx, h - 30); ctx.stroke();
  ctx.fillStyle = "#34d399"; ctx.textAlign = "center"; ctx.fillText("表面 " + scan.surface_sample, sx, 12);
  if (pendingSurfaceSample !== null) {
    const px2 = xOfSample(pendingSurfaceSample, w);
    ctx.strokeStyle = "#fbbf24"; ctx.setLineDash([2, 2]);
    ctx.beginPath(); ctx.moveTo(px2, 10); ctx.lineTo(px2, h - 30); ctx.stroke(); ctx.setLineDash([]);
  }

  // candidates
  for (const c of scan.candidates) {
    const x0 = xOfSample(c.start, w), x1 = xOfSample(c.end, w);
    const color = c.kind === "saturated" ? "#ff6b6b" : c.kind === "composite" ? "#c084fc" : c.kind === "split" ? "#34d399" : "#7aa2ff";
    ctx.strokeStyle = color; ctx.lineWidth = selectedCand === c.id ? 2.5 : 1.4;
    ctx.strokeRect(x0, 10, Math.max(2, x1 - x0), h - 40);
    if (selectedCand === c.id) {
      ctx.fillStyle = color; ctx.textAlign = "center"; ctx.font = "10px sans-serif";
      ctx.fillText(c.id, (x0 + x1) / 2, h - 2);
    }
  }
}

function drawDepth() {
  const cv = $("depth"); const w = cv.width, h = cv.height;
  const ctx = cv.getContext("2d");
  ctx.clearRect(0, 0, w, h);
  const scan = curScan();
  ctx.strokeStyle = "#3a4560"; ctx.beginPath(); ctx.moveTo(40, h / 2); ctx.lineTo(w - 20, h / 2); ctx.stroke();
  if (S.inverted) {
    ctx.fillStyle = "#ff8080"; ctx.font = "12px sans-serif"; ctx.textAlign = "left";
    ctx.fillText("深度轴已倒置，禁止输出深度结果", 44, h / 2 - 4);
    return;
  }
  const calib = S.calibs.find((c) => c.id === S.current_calib_id);
  for (let mm = 0; mm <= 24; mm += 4) {
    const n = scan.surface_sample + (mm * 2 / calib.velocity_ms) * 1000 * S.fs_mhz;
    if (n < 0 || n > S.n) continue;
    const x = xOfSample(n, w);
    ctx.fillStyle = "#5b688f"; ctx.font = "9px sans-serif"; ctx.textAlign = "center";
    ctx.fillText(mm + "mm", x, h - 4);
    ctx.strokeStyle = "#2a3550";
    ctx.beginPath(); ctx.moveTo(x, 4); ctx.lineTo(x, h / 2); ctx.stroke();
  }
  ctx.fillStyle = "#8b97b3"; ctx.textAlign = "left";
  ctx.fillText("距离（相对修正后表面，" + calib.name + " c=" + calib.velocity_ms + "m/s）", 42, 14);
}

function renderGates() {
  const box = $("gateEditors");
  box.innerHTML = "";
  for (const g of S.gates) {
    const div = document.createElement("div");
    div.className = "row";
    div.innerHTML = `<span style="width:52px" class="muted">${gateLabel(g.kind)}</span>`;
    const lo = document.createElement("input"); lo.type = "number"; lo.value = g.lo; lo.style.width = "62px";
    const span = document.createElement("span"); span.className = "muted"; span.textContent = "..";
    const hi = document.createElement("input"); hi.type = "number"; hi.value = g.hi; hi.style.width = "62px";
    const btn = document.createElement("button"); btn.textContent = "移动";
    btn.onclick = async () => {
      try {
        S = await postEvent({ type: "gate_moved", kind: g.kind, lo: Number(lo.value), hi: Number(hi.value) }, "调整闸门");
        render();
      } catch (e) { showErr(e.message); }
    };
    div.append(lo, span, hi, btn);
    box.appendChild(div);
  }
}

function renderCandidates() {
  const tb = $("candTable").querySelector("tbody");
  tb.innerHTML = "";
  const scan = curScan();
  $("scanTitle").textContent = `扫描位置 #${scan.id}`;
  $("scanMeta").textContent = `原始表面样本 ${scan.recorded_surface_sample} → 对齐 ${scan.surface_sample}；噪声底 σ=${fmt(scan.noise_floor * S.adc_scale, 0)} counts`;
  $("surfaceInput").value = scan.surface_sample;
  for (const c of scan.candidates) {
    const tr = document.createElement("tr");
    tr.className = "cand" + (selectedCand === c.id ? " sel" : "");
    const depth = S.inverted ? "倒置" : fmt(c.live_depth_mm, 2);
    tr.innerHTML = `<td><label><input type="checkbox" ${trackPick.has(c.id) ? "checked" : ""}/> ${c.id}</label>
        <div class="tag ${c.kind}">${kindLabel(c.kind)}</div></td>
      <td>${gateLabel(c.gate_kind)}</td>
      <td class="num">[${c.start},${c.end})</td>
      <td class="num">${c.peak}</td>
      <td class="num">${c.lower_bound ? "≥" : ""}${fmt(c.amplitude, 0)}</td>
      <td class="num">${depth}</td><td></td>`;
    tr.onclick = (e) => {
      if (e.target.tagName === "INPUT" || e.target.tagName === "BUTTON" || e.target.tagName === "SELECT") return;
      selectedCand = selectedCand === c.id ? null : c.id;
      render();
    };
    tr.querySelector("input[type=checkbox]").onchange = (e) => {
      e.stopPropagation();
      e.target.checked ? trackPick.add(c.id) : trackPick.delete(c.id);
      renderCandidates();
    };
    const ops = tr.lastElementChild;
    const vsel = document.createElement("select");
    for (const [v, label] of [["", "裁决…"], ["scatter", "真实散射"], ["overlap", "重叠回波"], ["surface", "表面"], ["bottom", "底面"]]) {
      const o = document.createElement("option"); o.value = v; o.textContent = label;
      if (c.verdict === v) o.selected = true;
      vsel.appendChild(o);
    }
    vsel.onchange = async () => {
      try {
        if (vsel.value) S = await postEvent({ type: "candidate_verdict", candidate_id: c.id, verdict: vsel.value }, "保留候选裁决");
        render();
      } catch (e) { showErr(e.message); render(); }
    };
    ops.appendChild(vsel);
    if (c.kind !== "saturated" && c.origin !== "split") {
      const sb = document.createElement("button");
      sb.textContent = "拆分";
      sb.title = "在候选区间中点波谷处拆分为两个候选";
      sb.style.marginLeft = "4px";
      sb.onclick = async (e) => {
        e.stopPropagation();
        const cut = Math.round((c.start + c.end) / 2);
        try {
          S = await postEvent({ type: "candidate_split", parent_id: c.id, cut }, "人工拆分宽回波");
          render();
        } catch (err2) { showErr(err2.message); }
      };
      ops.appendChild(sb);
    }
    tb.appendChild(tr);
  }
  $("trackInfo").textContent = trackPick.size ? `已勾选 ${trackPick.size} 个候选` : "";
}

function renderLog() {
  const box = $("log");
  box.innerHTML = "";
  for (const ev of [...S.events].reverse()) {
    const d = document.createElement("div");
    d.textContent = `#${ev.seq} ${ev.at_rfc3339} ${ev.event.type} ` +
      JSON.stringify(Object.fromEntries(Object.entries(ev.event).filter(([k]) => k !== "type"))) +
      (ev.note ? `  // ${ev.note}` : "");
    box.appendChild(d);
  }
}

function renderChrome() {
  $("fixtureMeta").textContent = `${S.n} 样本 @ ${S.fs_mhz}MHz · ADC ±${S.adc_max} · 5 个扫描位置`;
  const cal = $("calib");
  if (cal.options.length !== S.calibs.length || cal.dataset.init !== "1") {
    cal.innerHTML = "";
    for (const c of S.calibs) {
      const o = document.createElement("option");
      o.value = c.id; o.textContent = `${c.name} (${c.velocity_ms}m/s, 延迟${c.probe_delay_us}μs)`;
      cal.appendChild(o);
    }
    cal.dataset.init = "1";
  }
  cal.value = String(S.current_calib_id);
  $("thrMode").value = S.threshold_mode;
  $("thrValue").value = S.threshold_mode === "snr" ? S.threshold_snr : S.threshold_amp;
  const banner = $("banner");
  if (S.inverted) {
    banner.style.display = "block";
    banner.textContent = "深度轴倒置：" + (S.inverted_reason || "") + "。已禁止裁决/拆分/轨迹等深度结论，请先修正表面到达时刻。";
  } else {
    banner.style.display = "none";
  }
}

function render() {
  if (!S) return;
  renderChrome();
  drawMap();
  drawWave();
  drawDepth();
  renderGates();
  renderCandidates();
  renderLog();
}

function showErr(msg) {
  const box = $("err");
  box.textContent = msg;
  setTimeout(() => { box.textContent = ""; }, 5000);
}

async function refresh() {
  S = await api("/api/state");
  render();
}

$("wave").addEventListener("click", (e) => {
  const cv = $("wave");
  const rect = cv.getBoundingClientRect();
  const x = (e.clientX - rect.left) * cv.width / rect.width;
  const n = sampleAtX(x, cv.width);
  if (n >= 0 && n < S.n) { pendingSurfaceSample = n; $("surfaceInput").value = n; drawWave(); }
});

$("surfaceBtn").onclick = async () => {
  const sample = Number($("surfaceInput").value);
  try {
    S = await postEvent({ type: "surface_corrected", scan_id: selectedScan, sample }, "修正表面对齐");
    pendingSurfaceSample = null;
    render();
  } catch (e) { showErr(e.message); }
};

$("applyThr").onclick = async () => {
  try {
    S = await postEvent({ type: "threshold_changed", mode: $("thrMode").value, value: Number($("thrValue").value) }, "切换阈值");
    render();
  } catch (e) { showErr(e.message); }
};

$("calib").onchange = async () => {
  try {
    S = await postEvent({ type: "calibration_switched", calib_id: Number($("calib").value) }, "切换声速/探头延迟版本");
    render();
  } catch (e) { showErr(e.message); render(); }
};

$("trackBtn").onclick = async () => {
  const ids = [...trackPick];
  if (ids.length < 2) return showErr("至少勾选两个相邻位置的候选");
  try {
    S = await postEvent({ type: "track_grouped", track_id: S.next_track_id, candidate_ids: ids }, "组成缺陷轨迹");
    trackPick.clear();
    render();
  } catch (e) { showErr(e.message); }
};

$("exportBtn").onclick = () => { window.location.href = "/api/export"; };
$("fixtureBtn").onclick = () => { window.location.href = "/api/fixture.json"; };
$("resetBtn").onclick = async () => {
  try {
    S = await api("/api/reset", { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
    trackPick.clear(); selectedCand = null;
    render();
  } catch (e) { showErr(e.message); }
};
$("importFile").onchange = async (e) => {
  const file = e.target.files[0];
  if (!file) return;
  try {
    const text = await file.text();
    const doc = JSON.parse(text);
    if (!Array.isArray(doc.events)) throw new Error("文件缺少 events 数组");
    S = await api("/api/import", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ events: doc.events }) });
    trackPick.clear();
    render();
  } catch (err2) { showErr(err2.message); }
};

refresh().catch((e) => showErr(e.message));
