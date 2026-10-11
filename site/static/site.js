(function () {
  var reduce = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  // Agent tabs over the hero video.
  document.querySelectorAll("[data-tabs]").forEach(function (box) {
    var video = box.querySelector("video");
    var caption = box.querySelector("figcaption");
    var tabs = box.querySelectorAll("[role=tab]");
    // Each poster is its clip's first frame. Fetched ahead, it shows the moment a tab is
    // clicked and the clip then starts on the same picture, with no blank frame between.
    window.addEventListener("load", function () {
      tabs.forEach(function (t) { new Image().src = reduce ? t.dataset.still : t.dataset.poster; });
    });
    tabs.forEach(function (tab) {
      tab.addEventListener("click", function () {
        tabs.forEach(function (t) { t.setAttribute("aria-selected", t === tab); });
        video.poster = reduce ? tab.dataset.still : tab.dataset.poster;
        video.src = tab.dataset.src;
        caption.textContent = tab.dataset.caption;
        if (!reduce) video.play().catch(function () {});
      });
    });
  });

  // Install tabs.
  document.querySelectorAll("[data-tabs-install]").forEach(function (box) {
    var tabs = box.querySelectorAll("[role=tab]");
    tabs.forEach(function (tab) {
      tab.addEventListener("click", function () {
        tabs.forEach(function (t) { t.setAttribute("aria-selected", t === tab); });
        box.querySelectorAll(".pane").forEach(function (p) { p.hidden = p.dataset.pane !== tab.dataset.pane; });
      });
    });
  });

  // Videos play only while on screen. With reduced motion they show controls instead, and
  // a still from the middle of the clip rather than its first frame (load() brings the
  // poster back if autoplay had already started).
  var videos = document.querySelectorAll("video");
  if (reduce) {
    videos.forEach(function (v) {
      v.removeAttribute("autoplay");
      v.pause();
      v.controls = true;
      if (v.dataset.still) { v.poster = v.dataset.still; v.load(); }
    });
  } else if ("IntersectionObserver" in window) {
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (e.isIntersecting) { e.target.play().catch(function () {}); } else { e.target.pause(); }
      });
    }, { threshold: 0.35 });
    videos.forEach(function (v) { io.observe(v); });
  }

  // Each terminal fades in once its picture is ready, instead of showing as an empty dark box
  // first (style.css keeps it hidden until .ready). Same for the agent card screenshots.
  function ready(el) { el.classList.add("ready"); }
  videos.forEach(function (v) {
    var term = v.closest(".term");
    if (!term) return;
    var done = function () { ready(term); };
    v.addEventListener("loadeddata", done);
    var img = new Image();
    img.src = v.poster;
    if (img.decode) img.decode().then(done, done); else { img.onload = img.onerror = done; }
  });
  document.querySelectorAll(".agent-card img").forEach(function (img) {
    if (img.complete) { ready(img); return; }
    img.addEventListener("load", function () { ready(img); });
    img.addEventListener("error", function () { ready(img); });
  });

  // Star count from the GitHub API, shown from 20 up. The button works without it.
  var gh = document.querySelector("[data-stars]");
  if (gh && window.fetch) {
    fetch("https://api.github.com/repos/GregoryBolshakov/peekme")
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (d) {
        if (d && typeof d.stargazers_count === "number" && d.stargazers_count >= 20) {
          var c = gh.querySelector(".count");
          c.textContent = d.stargazers_count;
          c.hidden = false;
        }
      })
      .catch(function () {});
  }
})();
