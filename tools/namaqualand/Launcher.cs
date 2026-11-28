using System;
using System.Diagnostics;
using System.IO;
using System.IO.Compression;
using System.Security.Cryptography;
using System.Threading;

// Package.cs is generated alongside this source by package_windows.py.
internal static class Launcher
{
    private static string Digest(string file)
    {
        using (var input = File.OpenRead(file))
        using (var sha = SHA256.Create())
            return BitConverter.ToString(sha.ComputeHash(input)).Replace("-", "").ToLowerInvariant();
    }

    // Only paths created by the launcher, validated URLs, and fixed arguments
    // are passed to child processes. All path arguments terminate in a file name.
    private static string Quote(string path)
    {
        return "\"" + path + "\"";
    }

    private static int Run(string executable, string arguments, string directory)
    {
        var start = new ProcessStartInfo(executable, arguments);
        start.UseShellExecute = false;
        start.WorkingDirectory = directory;
        using (var child = Process.Start(start))
        {
            child.WaitForExit();
            return child.ExitCode;
        }
    }

    private static string Install(string root)
    {
        string installed = Path.Combine(root, Package.Sha256);
        if (File.Exists(Path.Combine(installed, "scene-viewer.exe")) &&
            File.Exists(Path.Combine(installed, "scene", "scene.json")))
            return installed;

        string temp = Path.Combine(root, Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(temp);
        try
        {
            string archive = Path.Combine(AppDomain.CurrentDomain.BaseDirectory, Package.FileName);
            if (!File.Exists(archive))
            {
                Console.WriteLine("Downloading Namaqualand " + Package.Version);
                archive = Path.Combine(temp, Package.FileName);
                string curl = Path.Combine(Environment.SystemDirectory, "curl.exe");
                if (Run(curl, "--fail --location --retry 3 --progress-bar --output " +
                        Quote(archive) + " " + Quote(Package.Url), temp) != 0)
                    throw new IOException("Download failed. Start the launcher again to retry.");
            }
            Console.WriteLine("Verifying " + Package.FileName);
            if (Digest(archive) != Package.Sha256)
                throw new InvalidDataException("The downloaded archive failed SHA-256 verification.");
            string unpacked = Path.Combine(temp, "unpacked");
            Console.WriteLine("Installing to " + installed);
            ZipFile.ExtractToDirectory(archive, unpacked);
            if (Directory.Exists(installed))
                Directory.Move(installed, Path.Combine(root, "incomplete-" + Guid.NewGuid().ToString("N")));
            Directory.Move(unpacked, installed);
            return installed;
        }
        finally { Directory.Delete(temp, true); }
    }

    public static int Main(string[] args)
    {
        try
        {
            string api = "d3d12";
            string mode = "walk";
            foreach (string arg in args)
            {
                switch (arg)
                {
                    case "--vulkan": api = "vulkan"; break;
                    case "--capture": mode = "capture"; break;
                    case "--check": mode = "check"; break;
                    default: throw new ArgumentException("Expected --vulkan, --capture or --check.");
                }
            }
            string root = Environment.GetEnvironmentVariable("SASHIKO_NAMAQUALAND_CACHE");
            if (String.IsNullOrWhiteSpace(root))
                root = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                                       "Sashiko", "Namaqualand");
            root = Path.GetFullPath(root);
            Directory.CreateDirectory(root);
            string install;
            using (var mutex = new Mutex(false, @"Local\Sashiko.Namaqualand." + Package.Sha256))
            {
                try { mutex.WaitOne(); }
                catch (AbandonedMutexException) { }
                try { install = Install(root); }
                finally { mutex.ReleaseMutex(); }
            }
            string captures = Path.Combine(root, "captures");
            Directory.CreateDirectory(captures);
            Console.WriteLine("WASD to walk, arrows to turn, R to play/pause the route.");
            Console.WriteLine("Home resets the view. L toggles full detail. F12 saves a PNG to " + captures);
            string options = "--walk-route " + Quote(Path.Combine(install, "walk.json"));
            if (mode == "capture")
                options = "--capture " + Quote(Path.Combine(captures, "rock-" + Guid.NewGuid().ToString("N") + ".png"));
            if (mode == "check") options = "--present-frames 3";
            return Run(Path.Combine(install, "scene-viewer.exe"),
                       "--api " + api + " " + Quote(Path.Combine(install, "scene")) + " rock " + options, captures);
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error.Message);
            if (!Console.IsInputRedirected && args.Length == 0)
            {
                Console.Error.WriteLine("Press Enter to close.");
                Console.ReadLine();
            }
            return 1;
        }
    }
}
