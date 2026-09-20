from PIL import Image
import numpy as np

board = Image.open('apps/desktop-ui/assets/master_cockpit_board.png')
arr = np.array(board)

def find_bounds(approx_l, approx_t, approx_r, approx_b):
    # Find exact left
    left = approx_l
    for x in range(approx_l - 15, approx_l + 15):
        mid_y = (approx_t + approx_b) // 2
        if arr[mid_y, x, 0] > 70 and arr[mid_y, x, 1] > 70:
            left = x
            break

    # Find exact right
    right = approx_r
    for x in range(approx_r + 15, approx_r - 15, -1):
        mid_y = (approx_t + approx_b) // 2
        if arr[mid_y, x, 0] > 70 and arr[mid_y, x, 1] > 70:
            right = x
            break

    # Find exact top
    top = approx_t
    for y in range(approx_t - 15, approx_t + 15):
        mid_x = (approx_l + approx_r) // 2
        if arr[y, mid_x, 0] > 60 and arr[y, mid_x, 1] > 60:
            top = y
            break

    # Find exact bottom
    bottom = approx_b
    for y in range(approx_b + 15, approx_b - 15, -1):
        mid_x = (approx_l + approx_r) // 2
        if arr[y, mid_x, 0] > 50 and arr[y, mid_x, 1] > 50:
            bottom = y
            break

    return (left, top, right + 1, bottom + 1)

# Window 1
b1 = (56, 19, 497, 560) # 441 x 541
w1 = board.crop(b1)
w1.save('apps/desktop-ui/assets/window_01_activation.png')
print('W1 cropped:', b1, w1.size)

# Window 2
# approx (555, 19, 982, 565)
b2 = find_bounds(555, 19, 982, 565)
w2 = board.crop(b2)
w2.save('apps/desktop-ui/assets/window_02_active_dashboard.png')
print('W2 cropped:', b2, w2.size)

# Window 3
# approx (1041, 19, 1483, 565)
b3 = find_bounds(1041, 19, 1483, 565)
w3 = board.crop(b3)
w3.save('apps/desktop-ui/assets/window_03_standby.png')
print('W3 cropped:', b3, w3.size)

# Window 4
b4 = find_bounds(57, 608, 747, 966)
w4 = board.crop(b4)
w4.save('apps/desktop-ui/assets/window_04_health_guardian.png')
print('W4 cropped:', b4, w4.size)

# Window 5
b5 = find_bounds(790, 606, 1480, 963)
w5 = board.crop(b5)
w5.save('apps/desktop-ui/assets/window_05_settings.png')
print('W5 cropped:', b5, w5.size)
