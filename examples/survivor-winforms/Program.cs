using Sashiko.WinForms;

static class Program
{
    [STAThread]
    static void Main(string[] args)
    {
        Application.SetUnhandledExceptionMode(UnhandledExceptionMode.ThrowException);
        ApplicationConfiguration.Initialize();
        bool smoke = args.Contains("--smoke");
        using var form = new Form { Text = "Survivor scene preview", ClientSize = new Size(960, 600) };
        var game = new Survivor();
        var view = new SashikoView(game) { Dock = DockStyle.Fill, Paused = true };
        var tools = new FlowLayoutPanel { Dock = DockStyle.Top, AutoSize = true };
        var play = new Button { Text = "Play", AutoSize = true };
        var step = new Button { Text = "Step", AutoSize = true };
        var x = Coordinate();
        var z = Coordinate();
        var place = new Button { Text = "Place player", AutoSize = true, Enabled = false };
        var status = new Label { AutoSize = true, Anchor = AnchorStyles.Left };
        tools.Controls.AddRange([play, step, Label("X"), x, Label("Z"), z, place, status]);
        form.Controls.Add(view);
        form.Controls.Add(tools);
        play.Click += (_, _) => {
            view.Paused = !view.Paused;
            play.Text = view.Paused ? "Play" : "Pause";
            step.Enabled = place.Enabled = view.Paused;
            if (!view.Paused) view.Focus();
        };
        step.Click += (_, _) => view.Step();
        place.Click += (_, _) => status.Text = game.Place((float)x.Value, (float)z.Value) ? "" : "Position is blocked";
        int frames = 0;
        view.Presented += () => {
            if (frames++ == 0)
            {
                place.Enabled = true;
                x.Value = (decimal)game.Position(0);
                z.Value = (decimal)game.Position(1);
            }
            if (!smoke) return;
            if (frames == 1)
            {
                if (game.Tick != 0 || !game.Place(game.Position(0) + 0.1f, game.Position(1)))
                    throw new Exception("Paused placement failed");
                view.Step();
            }
            else if (frames == 3)
            {
                if (game.Tick != 1) throw new Exception("Redraw advanced simulation");
                // Exercise teardown and a new child on the same UI thread.
                frames = 0;
                form.BeginInvoke(() => { form.Controls.Remove(view); view.Dispose();
                    var reopened = new SashikoView(game) { Dock = DockStyle.Fill, Paused = true };
                    reopened.Presented += () => {
                        if (game.Tick != 0) throw new Exception("Reopened session retained old state");
                        form.Close();
                    };
                    form.Controls.Add(reopened);
                    tools.BringToFront();
                });
            }
        };
        using var timeout = new System.Windows.Forms.Timer { Interval = 60000 };
        if (smoke) { timeout.Tick += (_, _) => throw new TimeoutException("Frame was not presented"); timeout.Start(); }
        Application.Run(form);
    }

    static NumericUpDown Coordinate() => new() { Width = 78, Minimum = -128, Maximum = 128,
        DecimalPlaces = 2, Increment = 0.25m };
    static Label Label(string text) => new() { Text = text, AutoSize = true, Anchor = AnchorStyles.Left };
}
