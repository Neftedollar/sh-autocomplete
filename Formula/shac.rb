class Shac < Formula
  desc "Local shell autocomplete engine for zsh and bash"
  homepage "https://github.com/Neftedollar/sh-autocomplete"
  url "https://github.com/Neftedollar/sh-autocomplete/archive/refs/tags/v0.1.1.tar.gz"
  sha256 "a55c399e403229861676e4f982f1b14bbd670989029b782434ca54f0c135c1fb"
  license "MIT"
  head "https://github.com/Neftedollar/sh-autocomplete.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
    pkgshare.install "shell"
  end

  def caveats
    <<~EOS
      Install shell integration with:
        shac install --shell zsh --edit-rc

      Start and inspect the daemon with:
        shac daemon start
        shac doctor
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/shac --version")
    assert_match "stopped", shell_output("#{bin}/shac daemon status")
  end
end
