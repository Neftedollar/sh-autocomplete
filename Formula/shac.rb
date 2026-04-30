class Shac < Formula
  desc "Local shell autocomplete engine for bash, zsh, and fish"
  homepage "https://github.com/Neftedollar/sh-autocomplete"
  url "https://github.com/Neftedollar/sh-autocomplete/archive/refs/tags/v0.5.1.tar.gz"
  sha256 "3f769526bfbc0ce17efda8198b3998eb4a990eb7923fbaae0a45a76e99467171"
  license "MIT"
  head "https://github.com/Neftedollar/sh-autocomplete.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
    pkgshare.install "shell"
  end

  service do
    run [opt_bin/"shacd"]
    keep_alive true
    log_path var/"log/shac.log"
    error_log_path var/"log/shac.log"
  end

  def caveats
    <<~EOS
      Install shell integration with:
        shac install --shell zsh --edit-rc

      Start the daemon (auto-restarts on login via launchd):
        brew services start shac

      Or start manually without launchd:
        shac daemon start

      Check status:
        shac doctor
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/shac --version")
    assert_match "stopped", shell_output("#{bin}/shac daemon status")
  end
end
