using System.Runtime.InteropServices;
using Sashiko.WinForms;

sealed class Survivor : IGameSurface
{
    private uint id;
    private const string Dll = "survivor.dll";
    [DllImport(Dll)] static extern uint survivor_open(uint seed);
    [DllImport(Dll)] static extern int survivor_attach(uint id, nint hwnd, uint width, uint height,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string catalog);
    [DllImport(Dll)] static extern int survivor_close(uint id);
    [DllImport(Dll)] static extern int survivor_focus(uint id, int active);
    [DllImport(Dll)] static extern int survivor_button(uint id, int mouse, ushort code, int down);
    [DllImport(Dll)] static extern int survivor_poll_pad(uint id, uint slot);
    [DllImport(Dll)] static extern int survivor_advance(uint id, uint micros);
    [DllImport(Dll)] static extern int survivor_step(uint id);
    [DllImport(Dll)] static extern int survivor_draw(uint id, uint width, uint height);
    [DllImport(Dll)] static extern int survivor_place(uint id, float x, float z);
    [DllImport(Dll)] static extern float survivor_position(uint id, uint axis);
    [DllImport(Dll)] static extern double survivor_tick(uint id);
    [DllImport(Dll)] static extern nint survivor_error();

    static int Check(int result) => result >= 0 ? result :
        throw new InvalidOperationException(Marshal.PtrToStringUTF8(survivor_error()));
    public void Attach(nint hwnd, Size size)
    {
        id = survivor_open(506);
        if (id == 0) Check(-1);
        var catalog = Path.Combine(AppContext.BaseDirectory, "assets/survivor/characters/catalog.json");
        try { Check(survivor_attach(id, hwnd, (uint)size.Width, (uint)size.Height, catalog)); }
        catch { Detach(); throw; }
    }
    public void Detach() { var closing = id; id = 0; Check(survivor_close(closing)); }
    public void Focus(bool active) => Check(survivor_focus(id, active ? 1 : 0));
    public void Button(bool mouse, ushort code, bool down) => Check(survivor_button(id, mouse ? 1 : 0, code, down ? 1 : 0));
    public void PollDevices() => Check(survivor_poll_pad(id, 0));
    public void Advance(uint micros) => Check(survivor_advance(id, micros));
    public void Step() => Check(survivor_step(id));
    public bool Draw(Size size) => Check(survivor_draw(id, (uint)size.Width, (uint)size.Height)) == 0;
    public bool Place(float x, float z) => Check(survivor_place(id, x, z)) == 0;
    public float Position(uint axis) => survivor_position(id, axis);
    public double Tick => survivor_tick(id);
}
