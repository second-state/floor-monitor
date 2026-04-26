// Floor Monitor — Dashboard live updates via SSE + preview polling

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
                // Append cache-buster
                const base = src.split("?")[0];
                img.src = base + "?t=" + Date.now();
            }
        });
    }

    // --- Init ---
    connectSSE();
    setInterval(pollPreviews, PREVIEW_POLL_MS);
})();
