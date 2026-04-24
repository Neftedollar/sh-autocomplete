class Shac < Formula
  desc "Local shell autocomplete engine for zsh and bash"
  homepage "https://github.com/Neftedollar/sh-autocomplete"
  url "https://github.com/Neftedollar/sh-autocomplete/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "0cdb3f54c24db1bbc5cffc06cd96689382ef366a9ce066d10ac477a44ec63f65"
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
