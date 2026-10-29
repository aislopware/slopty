return ui.scene { height = 300, draw = function(s)
          s:environment { asset = "sky.png", intensity = 1.5 }
          s:model { asset = "tri.glb", fit = 1, rotate = { 0, 90, 0 } }
          s:model { asset = "tri.glb", at = { 3, 0, 0 }, color = "#00ff00", metal = 1 }
          s:box { at = { 0, -1, 0 }, texture = "wood.png" }
          s:sprite { at = { 0, 2, 0 }, size = 64, color = "#ffeedd", glow = 2 }
        end }
