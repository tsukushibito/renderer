using System.Diagnostics;

namespace Sashiko.WinForms;

/// A game implements this boundary once. Forms can then place its view like any other control.
public interface IGameSurface
{
    void Attach(nint hwnd, Size size);
    void Detach();
    void Focus(bool active);
    void Button(bool mouse, ushort code, bool pressed);
    void PollDevices();
    void Advance(uint microseconds);
    void Step();
    bool Draw(Size size);
}

public sealed class SashikoView : Control
{
    private readonly IGameSurface game;
    private readonly System.Windows.Forms.Timer timer = new() { Interval = 15 };
    private readonly Stopwatch elapsed = new();
    private bool attached;
    private bool pending;
    private bool paused;
    private bool inputEnabled = true;
    public event Action? Presented;

    public SashikoView(IGameSurface game)
    {
        this.game = game;
        TabStop = true;
        SetStyle(ControlStyles.Selectable | ControlStyles.Opaque | ControlStyles.UserPaint, true);
        timer.Tick += (_, _) => Frame();
    }

    [System.ComponentModel.DefaultValue(false)]
    public bool Paused
    {
        get => paused;
        set { paused = value; elapsed.Restart(); }
    }

    [System.ComponentModel.DefaultValue(true)]
    public bool InputEnabled
    {
        get => inputEnabled;
        set { inputEnabled = value; if (attached) game.Focus(value && Focused); }
    }

    public void Step()
    {
        if (!attached || !Paused) return;
        game.Step();
        Invalidate();
    }

    private void Frame()
    {
        if (!Visible || ClientSize.Width == 0 || ClientSize.Height == 0 ||
            FindForm()?.WindowState == FormWindowState.Minimized)
        {
            elapsed.Restart();
            return;
        }
        if (!attached)
        {
            game.Attach(Handle, ClientSize);
            attached = true;
            game.Focus(InputEnabled && Focused);
            elapsed.Restart();
        }
        game.PollDevices();
        if (!pending)
        {
            if (!Paused) game.Advance((uint)Math.Min(uint.MaxValue, elapsed.Elapsed.TotalMicroseconds));
            elapsed.Restart();
        }
        pending = !game.Draw(ClientSize);
        if (!pending) Presented?.Invoke();
    }

    protected override void OnHandleCreated(EventArgs e)
    {
        base.OnHandleCreated(e);
        elapsed.Restart();
        timer.Start();
    }

    protected override void DestroyHandle()
    {
        try { Detach(); }
        finally { base.DestroyHandle(); }
    }

    private void Detach()
    {
        timer.Stop();
        if (!attached) return;
        attached = pending = false;
        game.Detach();
    }

    protected override void Dispose(bool disposing)
    {
        base.Dispose(disposing);
        if (disposing) timer.Dispose();
    }

    protected override void OnGotFocus(EventArgs e) { base.OnGotFocus(e); if (attached) game.Focus(InputEnabled); }
    protected override void OnLostFocus(EventArgs e) { if (attached) game.Focus(false); base.OnLostFocus(e); }
    protected override bool IsInputKey(Keys keyData) =>
        (keyData & Keys.KeyCode) is Keys.Left or Keys.Right or Keys.Up or Keys.Down || base.IsInputKey(keyData);

    // Translate Windows scan codes to physical USB keyboard usages for the supported controls.
    private static ushort Usage(int scan) => scan switch
    {
        0x01 => 41, >= 0x02 and <= 0x0b => (ushort)(30 + scan - 2),
        0x10 => 20, 0x11 => 26, 0x12 => 8, 0x13 => 21, 0x14 => 23, 0x15 => 28,
        0x16 => 24, 0x17 => 12, 0x18 => 18, 0x19 => 19,
        0x1e => 4, 0x1f => 22, 0x20 => 7, 0x21 => 9, 0x22 => 10, 0x23 => 11,
        0x24 => 13, 0x25 => 14, 0x26 => 15, 0x2c => 29, 0x2d => 27, 0x2e => 6,
        0x2f => 25, 0x30 => 5, 0x31 => 17, 0x32 => 16, 0x1c => 40, 0x39 => 44,
        0x14b => 80, 0x14d => 79, 0x148 => 82, 0x150 => 81, 0x0e => 42, 0x0f => 43,
        _ => 0
    };

    protected override void WndProc(ref Message m)
    {
        // A native parent can also initiate destruction of its children.
        if (m.Msg == 0x0002) Detach(); // WM_DESTROY
        const int KeyDown = 0x100, KeyUp = 0x101;
        if (attached && InputEnabled && Focused && (m.Msg == KeyDown || m.Msg == KeyUp))
        {
            ushort usage = Usage((int)((m.LParam.ToInt64() >> 16) & 0x1ff));
            if (usage != 0) game.Button(false, usage, m.Msg == KeyDown);
        }
        base.WndProc(ref m);
    }

    private void MouseButton(MouseEventArgs e, bool down)
    {
        if (down) Focus();
        ushort button = e.Button switch
        {
            MouseButtons.Left => 0, MouseButtons.Middle => 1, MouseButtons.Right => 2,
            MouseButtons.XButton1 => 3, MouseButtons.XButton2 => 4, _ => ushort.MaxValue
        };
        if (attached && InputEnabled && Focused && button != ushort.MaxValue) game.Button(true, button, down);
    }
    protected override void OnMouseDown(MouseEventArgs e) { MouseButton(e, true); base.OnMouseDown(e); }
    protected override void OnMouseUp(MouseEventArgs e) { MouseButton(e, false); base.OnMouseUp(e); }
}
