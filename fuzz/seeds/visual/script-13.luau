return ui.column {
          ui.chart { type = "line", height = 200, x = { "Mon", "Tue", "Wed" }, unit = "ms",
            series = { { name = "p95", values = { 120, 180, 140 } }, { name = "p99", values = { 200, nil, 260 } } } },
          ui.table { columns = { "Region", "p95" }, rows = { { "us-east", 182 }, { "eu-west", 140 } } },
          ui.row { ui.stat { value = "182 ms", label = "p95" }, ui.stat { value = 99.2, label = "uptime", tone = "success" } },
        }
