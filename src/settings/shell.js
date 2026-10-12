    function selectPane(paneId, options = {}) {
      activePane = paneId;
      if (options.byUser) paneChosen = true;
      renderPanes();
      renderReadinessList(latestReport);
    }

    function renderPanes() {
      const searching = searchQuery !== "";
      document.getElementById("content").classList.toggle("searching", searching);

      PANES.forEach((pane) => {
        document.getElementById(`rail-${pane.id}`).dataset.active = String(!searching && pane.id === activePane);
        document.getElementById(`pane-${pane.id}`).hidden = !searching && pane.id !== activePane;
      });

      const pane = PANES.find((entry) => entry.id === activePane) || PANES[0];
      const paneIcon = document.getElementById("pane-icon");
      paneIcon.hidden = searching;
      paneIcon.dataset.group = pane.id;
      paneIcon.innerHTML = iconSvg(pane.icon, 18);
      document.getElementById("pane-title").textContent = searching ? "Results" : pane.title;
      document.getElementById("summary").textContent = searching ? `for “${searchQuery}”` : pane.subtitle;

      renderSearch();
    }

    function renderSearch() {
      const query = searchQuery.toLowerCase();
      let matches = 0;

      document.querySelectorAll(".row").forEach((row) => {
        const words = `${row.textContent} ${row.dataset.keywords || ""}`.toLowerCase();
        const hit = query === "" || words.includes(query);
        row.classList.toggle("search-miss", !hit);
        if (hit && !row.hidden) matches += 1;
      });

      document.querySelectorAll(".card").forEach((card) => {
        const rows = [...card.querySelectorAll(".row")];
        card.classList.toggle(
          "search-miss",
          query !== "" && !rows.some((row) => !row.hidden && !row.classList.contains("search-miss"))
        );
      });

      PANES.forEach((pane) => {
        const section = document.getElementById(`pane-${pane.id}`);
        const rows = [...section.querySelectorAll(".row")];
        section.classList.toggle(
          "search-miss",
          query !== "" && !rows.some((row) => !row.hidden && !row.classList.contains("search-miss"))
        );
      });

      document.getElementById("search-empty").hidden = query === "" || matches > 0;
    }

    function setSearchQuery(query) {
      searchQuery = query.trim();
      renderPanes();
    }

    function renderBadges(report) {
      const outstanding = blockersOf(report);

      PANES.forEach((pane) => {
        const count = outstanding.filter((item) => item.pane === pane.id).length;
        const badge = document.getElementById(`badge-${pane.id}`);
        badge.hidden = count === 0;
        badge.innerHTML = count ? `${iconSvg("alert", 12)}<span>${count}</span>` : "";
        badge.setAttribute("aria-label", `${count} to fix`);
      });
    }

