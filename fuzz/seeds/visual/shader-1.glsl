void mainImage(out vec4 fragColor, in vec2 fragCoord) {
                vec2 uv = fragCoord / iResolution.xy;
                fragColor = vec4(uv, 0.5 + 0.5 * sin(iTime * speed + iCamera.x), 1.0);
              }
