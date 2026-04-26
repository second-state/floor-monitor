// Floor Monitor — Dashboard live updates via SSE + preview polling + controls

(function () {
    "use strict";

    const PREVIEW_POLL_MS = 1000;
    const statusDot = document.getElementById("connection-status");
    const resultsContainer = document.getElementById("results-container");

    // --- SSE: live analysis results ---
    let evtSource = null;

    function connectSSE() {
        if (evtSource) evtSource.close();
        evtSource = new EventSource("/api/events");

        evtSource.onopen = function () {
            if (statusDot) {
                statusDot.classList.remove("disconnected");
                statusDot.classList.add("connected");
                statusDot.title = "Connected";
            }
        };

        evtSource.onmessage = function (event) {
            try {
                const data = JSON.parse(event.data);
                prependResult(data);
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
        };
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

        // Cap displayed results
        while (resultsContainer.children.length > 100) {
            resultsContainer.removeChild(resultsContainer.lastChild);
        }
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

    // --- Init ---
    connectSSE();
    setInterval(pollPreviews, PREVIEW_POLL_MS);
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
