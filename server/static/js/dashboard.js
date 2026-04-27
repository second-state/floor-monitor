// Floor Monitor — Dashboard live updates via SSE + preview polling + controls

(function () {
    "use strict";

    const PREVIEW_POLL_MS = 1000;
    // Backfill summaries periodically and on reconnect to recover from any
    // events lost while the SSE connection was down (e.g. Cloudflare idle
    // timeout). Frame results are transient and don't need backfill.
    const SUMMARY_POLL_MS = 60000;
    const SSE_RECONNECT_MS = 3000;
    const statusDot = document.getElementById("connection-status");
    const resultsContainer = document.getElementById("results-container");
    const summariesContainer = document.getElementById("summaries-container");

    // --- SSE: live analysis results ---
    let evtSource = null;
    let reconnectTimer = null;

    function connectSSE() {
        if (evtSource) {
            evtSource.close();
            evtSource = null;
        }
        evtSource = new EventSource("/api/events");

        evtSource.onopen = function () {
            if (statusDot) {
                statusDot.classList.remove("disconnected");
                statusDot.classList.add("connected");
                statusDot.title = "Connected";
            }
            // Backfill summaries that may have been emitted while disconnected.
            refreshSummaries();
        };

        evtSource.onmessage = function (event) {
            try {
                const data = JSON.parse(event.data);
                if (data.kind === "summary") {
                    prependSummary(data);
                } else {
                    // kind === "result" or legacy untagged payload
                    prependResult(data);
                }
            } catch (e) {
                console.warn("SSE parse error:", e);
            }
        };

        evtSource.onerror = function () {
            if (statusDot) {
                statusDot.classList.remove("connected");
                statusDot.classList.add("disconnected");
                statusDot.title = "Disconnected — reconnecting...";
            }
            // Close and reconnect after a short delay rather than relying on
            // the browser's auto-reconnect, which can hammer the server.
            if (evtSource) {
                evtSource.close();
                evtSource = null;
            }
            if (reconnectTimer) clearTimeout(reconnectTimer);
            reconnectTimer = setTimeout(connectSSE, SSE_RECONNECT_MS);
        };
    }

    // Fetch the current summaries buffer and reconcile with what's on screen.
    // Only adds entries we don't already display (deduped by time + window).
    function refreshSummaries() {
        if (!summariesContainer) return;
        fetch("/api/summaries")
            .then(function (r) { return r.json(); })
            .then(function (list) {
                if (!Array.isArray(list)) return;
                const seen = new Set();
                summariesContainer.querySelectorAll(".summary-entry").forEach(function (el) {
                    seen.add(el.getAttribute("data-key") || "");
                });
                // The endpoint returns newest first; iterate reversed so we
                // prepend in chronological order (oldest of the new batch
                // first → newest ends up on top).
                for (let i = list.length - 1; i >= 0; i--) {
                    const s = list[i];
                    const key = (s.time || "") + "|" + (s.window_min || 0);
                    if (!seen.has(key)) prependSummary(s);
                }
            })
            .catch(function (e) {
                console.warn("Summary refresh failed:", e);
            });
    }

    function prependResult(r) {
        if (!resultsContainer) return;

        // Remove "waiting" placeholder
        const placeholder = resultsContainer.querySelector(".muted");
        if (placeholder) placeholder.remove();

        const entry = document.createElement("div");
        entry.className = "result-entry";
        entry.innerHTML =
            '<div class="result-header">' +
            "<strong>frame=" + r.frame_no + "</strong> | " +
            r.time + " | " +
            "infer=" + (r.infer_secs || 0).toFixed(2) + "s | " +
            "model=" + (r.model || "?") + " | " +
            "camera=" + (r.camera_id || "?") +
            "</div>" +
            '<div class="result-text">' + escapeHtml(r.text || "") + "</div>";

        resultsContainer.prepend(entry);

        // Update frame count in camera card
        if (r.camera_id && r.frame_no) {
            var fc = document.getElementById("frame-count-" + r.camera_id);
            if (fc) fc.textContent = "Frame #" + r.frame_no;
        }

        // Cap displayed results
        while (resultsContainer.children.length > 100) {
            resultsContainer.removeChild(resultsContainer.lastChild);
        }
    }

    function prependSummary(s) {
        if (!summariesContainer) return;

        const placeholder = summariesContainer.querySelector(".muted");
        if (placeholder) placeholder.remove();

        const key = (s.time || "") + "|" + (s.window_min || 0);
        // Skip if an entry with the same key is already shown.
        const existing = summariesContainer.querySelector(
            '.summary-entry[data-key="' + cssEscape(key) + '"]'
        );
        if (existing) return;

        const entry = document.createElement("div");
        entry.className = "summary-entry";
        entry.setAttribute("data-key", key);
        entry.innerHTML =
            '<div class="summary-header">' +
            "<strong>" + escapeHtml(s.time || "") + "</strong> · last " +
            (s.window_min || 0) + " min" +
            "</div>" +
            '<div class="summary-text">' + escapeHtml(s.text || "") + "</div>";

        summariesContainer.prepend(entry);

        // Cap displayed summaries
        while (summariesContainer.children.length > 20) {
            summariesContainer.removeChild(summariesContainer.lastChild);
        }
    }

    function cssEscape(s) {
        if (window.CSS && CSS.escape) return CSS.escape(s);
        return String(s).replace(/["\\]/g, "\\$&");
    }

    function escapeHtml(s) {
        const d = document.createElement("div");
        d.textContent = s;
        return d.innerHTML;
    }

    // --- Preview polling ---
    function pollPreviews() {
        const imgs = document.querySelectorAll(".preview-img");
        imgs.forEach(function (img) {
            const src = img.getAttribute("src");
            if (src) {
                const base = src.split("?")[0];
                img.src = base + "?t=" + Date.now();
            }
        });
    }

    // --- Tabs ---
    function initTabs() {
        const buttons = document.querySelectorAll(".tab-btn");
        const panels = document.querySelectorAll(".tab-panel");
        if (buttons.length === 0) return;

        buttons.forEach(function (btn) {
            btn.addEventListener("click", function () {
                const target = btn.getAttribute("data-tab");
                buttons.forEach(function (b) {
                    const on = b === btn;
                    b.classList.toggle("active", on);
                    b.setAttribute("aria-selected", on ? "true" : "false");
                });
                panels.forEach(function (p) {
                    p.classList.toggle("active", p.id === "tab-" + target);
                });
            });
        });
    }

    // --- Init ---
    initTabs();
    connectSSE();
    setInterval(pollPreviews, PREVIEW_POLL_MS);
    setInterval(refreshSummaries, SUMMARY_POLL_MS);
})();

// --- Controls (global scope for onclick handlers) ---

function showResult(elemId, text, isError) {
    var el = document.getElementById(elemId);
    if (!el) return;
    el.textContent = text;
    el.className = "control-result" + (isError ? " error" : " success");
    // Auto-clear after 10 seconds
    setTimeout(function () {
        el.textContent = "";
        el.className = "control-result";
    }, 10000);
}

function askQuestion() {
    var input = document.getElementById("ask-input");
    var btn = document.getElementById("ask-btn");
    var question = input.value.trim();
    if (!question) return;

    btn.disabled = true;
    btn.textContent = "Thinking...";
    showResult("ask-result", "Analyzing...", false);

    fetch("/api/ask", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ question: question }),
    })
        .then(function (r) { return r.json(); })
        .then(function (data) {
            if (data.error) {
                showResult("ask-result", data.error, true);
            } else {
                var text = data.answer || "(empty response)";
                if (data.infer_secs) {
                    text += " (" + data.infer_secs.toFixed(1) + "s)";
                }
                showResult("ask-result", text, false);
            }
        })
        .catch(function (e) {
            showResult("ask-result", "Request failed: " + e, true);
        })
        .finally(function () {
            btn.disabled = false;
            btn.textContent = "Ask";
        });
}

function sendPtz(direction) {
    sendCommand("ptz", { direction: direction });
}

function sendCommand(action, params) {
    showResult("ptz-result", "Sending " + action + "...", false);

    fetch("/api/command", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ action: action, params: params || {} }),
    })
        .then(function (r) { return r.json(); })
        .then(function (data) {
            if (data.ok) {
                showResult("ptz-result", action + " sent to " + data.camera_id, false);
            } else {
                showResult("ptz-result", data.error || "Command failed", true);
            }
        })
        .catch(function (e) {
            showResult("ptz-result", "Request failed: " + e, true);
        });
}

function downloadSnapshot() {
    // Find the first camera preview image and open its snapshot URL
    var img = document.querySelector(".preview-img");
    if (img) {
        var src = img.getAttribute("src").split("?")[0];
        window.open(src, "_blank");
    } else {
        alert("No camera connected");
    }
}
