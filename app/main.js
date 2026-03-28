const tauri = window.__TAURI__;

if (!tauri?.core?.invoke || !tauri?.event?.listen || !tauri?.window?.getCurrentWindow) {
  throw new Error("Tauri API 未注入，当前静态前端无法连接后端");
}

const { invoke } = tauri.core;
const { listen } = tauri.event;
const appWindow = tauri.window.getCurrentWindow();

const elements = {
  logs: document.querySelector("#logs"),
  logCount: document.querySelector("#log-count"),
  statusText: document.querySelector("#status-text"),
  modePill: document.querySelector("#mode-pill"),
  configPath: document.querySelector("#config-path"),
  configFields: document.querySelector("#config-fields"),
  errorBanner: document.querySelector("#error-banner"),
  errorText: document.querySelector("#error-text"),
  buttons: {
    save: document.querySelector("#save-btn"),
    dryRun: document.querySelector("#dry-run-btn"),
    start: document.querySelector("#start-btn"),
    pause: document.querySelector("#pause-btn"),
    resume: document.querySelector("#resume-btn"),
    stop: document.querySelector("#stop-btn"),
    hide: document.querySelector("#hide-btn"),
  },
  metrics: {
    seeding: document.querySelector("#metric-seeding"),
    saturated: document.querySelector("#metric-saturated"),
    free: document.querySelector("#metric-free"),
    pending: document.querySelector("#metric-pending"),
    freeCount: document.querySelector("#metric-free-count"),
    filtered: document.querySelector("#metric-filtered"),
    planned: document.querySelector("#metric-planned"),
    roughPlanned: document.querySelector("#metric-rough-planned"),
    duplicates: document.querySelector("#metric-duplicates"),
    added: document.querySelector("#metric-added"),
  },
  fields: {
    byrUsername: document.querySelector("#byr-username"),
    byrPassword: document.querySelector("#byr-password"),
    qbHost: document.querySelector("#qb-host"),
    qbUsername: document.querySelector("#qb-username"),
    qbPassword: document.querySelector("#qb-password"),
    qbDownloadPath: document.querySelector("#qb-download-path"),
    downloadBudgetGb: document.querySelector("#download-budget"),
    includeCategories: document.querySelector("#include-categories"),
  },
};

function formatBytes(bytes) {
  const gib = 1024 ** 3;
  const tib = gib * 1024;
  if (!bytes) {
    return "0 GiB";
  }
  if (bytes >= tib) {
    return `${(bytes / tib).toFixed(2)} TiB`;
  }
  return `${(bytes / gib).toFixed(1)} GiB`;
}

function appendLog(line) {
  const entry = document.createElement("div");
  entry.className = "log-line";
  entry.textContent = line;
  elements.logs.appendChild(entry);
  elements.logCount.textContent = `${elements.logs.childElementCount} 条`;
  elements.logs.scrollTop = elements.logs.scrollHeight;
}

function replaceLogs(lines) {
  elements.logs.innerHTML = "";
  for (const line of lines) {
    appendLog(line);
  }
}

function fillForm(config) {
  elements.fields.byrUsername.value = config.byrUsername ?? "";
  elements.fields.byrPassword.value = config.byrPassword ?? "";
  elements.fields.qbHost.value = config.qbHost ?? "";
  elements.fields.qbUsername.value = config.qbUsername ?? "";
  elements.fields.qbPassword.value = config.qbPassword ?? "";
  elements.fields.qbDownloadPath.value = config.qbDownloadPath ?? "";
  elements.fields.downloadBudgetGb.value = config.downloadBudgetGb ?? "";
  elements.fields.includeCategories.value = config.includeCategories ?? "";
}

function collectForm() {
  return {
    byrUsername: elements.fields.byrUsername.value,
    byrPassword: elements.fields.byrPassword.value,
    qbHost: elements.fields.qbHost.value,
    qbUsername: elements.fields.qbUsername.value,
    qbPassword: elements.fields.qbPassword.value,
    qbDownloadPath: elements.fields.qbDownloadPath.value,
    downloadBudgetGb: elements.fields.downloadBudgetGb.value,
    includeCategories: elements.fields.includeCategories.value,
  };
}

function renderSnapshot(snapshot) {
  fillForm(snapshot.config);
  elements.statusText.textContent = snapshot.status;
  elements.configPath.textContent = snapshot.configPath;
  elements.modePill.textContent = snapshot.dryRun
    ? "Dry Run"
    : snapshot.running
      ? "执行中"
      : "待机";

  elements.metrics.seeding.textContent = snapshot.report.seedingCount;
  elements.metrics.saturated.textContent = snapshot.report.queueSaturated ? "是" : "否";
  elements.metrics.free.textContent = formatBytes(snapshot.report.freeSpaceBytes);
  elements.metrics.pending.textContent = formatBytes(snapshot.report.downloadingRemainingBytes);
  elements.metrics.freeCount.textContent = snapshot.report.freeCount;
  elements.metrics.filtered.textContent = snapshot.report.filteredCount;
  elements.metrics.planned.textContent = snapshot.report.plannedCount;
  elements.metrics.roughPlanned.textContent = snapshot.report.roughPlannedCount;
  elements.metrics.duplicates.textContent = snapshot.report.duplicateSkipCount;
  elements.metrics.added.textContent = `${snapshot.report.addedCount} / ${snapshot.report.skippedCount}`;

  elements.errorBanner.hidden = !snapshot.lastError;
  elements.errorText.textContent = snapshot.lastError ?? "";

  elements.configFields.disabled = snapshot.running;
  elements.buttons.save.disabled = snapshot.running;
  elements.buttons.dryRun.disabled = snapshot.running;
  elements.buttons.start.disabled = snapshot.running;
  elements.buttons.pause.disabled = !snapshot.running || snapshot.paused;
  elements.buttons.resume.disabled = !snapshot.running || !snapshot.paused;
  elements.buttons.stop.disabled = !snapshot.running;
}

function showCommandError(error) {
  elements.errorBanner.hidden = false;
  elements.errorText.textContent = String(error);
  appendLog(`命令失败: ${error}`);
}

async function invokeAndRender(command, payload = {}) {
  try {
    const snapshot = await invoke(command, payload);
    renderSnapshot(snapshot);
    return snapshot;
  } catch (error) {
    showCommandError(error);
    throw error;
  }
}

async function init() {
  await listen("monitor-log", (event) => {
    appendLog(event.payload.line);
  });

  await listen("monitor-state", (event) => {
    renderSnapshot(event.payload);
  });

  const snapshot = await invoke("load_app_state");
  replaceLogs(snapshot.logs);
  renderSnapshot(snapshot);

  elements.buttons.save.addEventListener("click", async () => {
    await invokeAndRender("save_config", { form: collectForm() });
  });

  elements.buttons.dryRun.addEventListener("click", async () => {
    await invokeAndRender("start_monitor", { dryRun: true });
  });

  elements.buttons.start.addEventListener("click", async () => {
    await invokeAndRender("start_monitor", { dryRun: false });
  });

  elements.buttons.pause.addEventListener("click", async () => {
    await invokeAndRender("pause_monitor");
  });

  elements.buttons.resume.addEventListener("click", async () => {
    await invokeAndRender("resume_monitor");
  });

  elements.buttons.stop.addEventListener("click", async () => {
    await invokeAndRender("stop_monitor");
  });

  elements.buttons.hide.addEventListener("click", async () => {
    appendLog("隐藏到托盘");
    await appWindow.hide();
  });
}

init().catch(showCommandError);
