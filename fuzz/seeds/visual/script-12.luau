return ui.scene { height = 300, spin = 0.3, draw = function(s)
          s:grid { size = 4, step = 1 }
          s:surface { x = { -2, 2 }, z = { -2, 2 }, detail = 12,
            colors = { "chart1", "chart3" },
            f = function(x, z) return math.sin(x) * math.cos(z) end }
          s:box { at = { 0, 1.5, 0 }, size = 0.5, color = "chart2" }
          s:point({ 1, 1, 1 }, { hint = "peak" })
          s:point { at = { -1, 1, -1 }, color = "chart4" }
          s:label { at = { 0, 2, 0 }, text = "top" }
        end }
