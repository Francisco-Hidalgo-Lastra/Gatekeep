//! Fictional accounts, messages, rules and blocked senders. Every name and
//! address is made up; companies use the usual demo names (Contoso, Fabrikam…).

use chrono::{DateTime, Duration, Local};

pub const ACCOUNTS: [(&str, &str); 2] = [
    ("alex.rivera@example.com", "Gmail"),
    ("alex.rivera@example.org", "Outlook"),
];

pub const BLOCKED: [&str; 7] = [
    "deals@megasavings.example",
    "noreply@prizecenter.example",
    "win@luckydraw.example",
    "promo@tailspintoys.com",
    "spamcorp.example",
    "cheap-pills.example",
    "crypto-gains.example",
];

#[derive(Clone, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub email: String,
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.name.is_empty() { f.write_str(&self.email) } else { write!(f, "{} <{}>", self.name, self.email) }
    }
}

impl Address {
    pub fn short(&self) -> &str {
        if self.name.is_empty() { &self.email } else { &self.name }
    }
}

#[derive(Clone)]
pub struct Message {
    pub uid: u32,
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    pub snippet: String,
    pub body: String,
    pub date: DateTime<Local>,
    pub unread: bool,
    /// Names of the display rules that matched (shown as chips).
    pub rules: Vec<&'static str>,
    pub highlight: bool,
    pub low_priority: bool,
}

fn addr(name: &str, email: &str) -> Address {
    Address { name: name.into(), email: email.into() }
}

fn me() -> Address {
    addr("Alex Rivera", ACCOUNTS[0].0)
}

/// Builder-ish helper: `minutes_ago` sets the date relative to now.
fn msg(
    uid: u32,
    minutes_ago: i64,
    from: Address,
    subject: &str,
    body: &str,
    unread: bool,
) -> Message {
    let snippet = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let snippet = snippet.chars().take(140).collect();
    Message {
        uid,
        from,
        to: vec![me()],
        cc: vec![],
        subject: subject.into(),
        snippet,
        body: body.into(),
        date: Local::now() - Duration::minutes(minutes_ago),
        unread,
        rules: vec![],
        highlight: false,
        low_priority: false,
    }
}

const H: i64 = 60;
const D: i64 = 24 * H;

pub fn messages() -> Vec<Message> {
    let mut v = vec![
        {
            let mut m = msg(
                1048,
                12,
                addr("Priya Natarajan", "priya.natarajan@contoso.com"),
                "Q4 roadmap review — slides attached",
                "Hi Alex,\n\nAttached are the slides for Thursday's roadmap review. The big changes since last time:\n\n  • Search indexing moves up to November\n  • The mobile beta slips two weeks (waiting on the design pass)\n  • We're dropping the legacy export format in Q1\n\nCould you take a look at section 3 before the meeting? I'd love your take on whether the timeline for the sync rewrite is realistic.\n\nThanks!\nPriya",
                true,
            );
            m.cc = vec![addr("Marcus Webb", "marcus.webb@contoso.com"), addr("Dana Okafor", "dana.okafor@contoso.com")];
            m.rules = vec!["Team"];
            m.highlight = true;
            m
        },
        msg(
            1047,
            47,
            addr("Woodgrove Bank", "alerts@woodgrovebank.com"),
            "Your monthly statement is ready",
            "Hello Alex,\n\nYour statement for the period ending the 15th is now available in online banking.\n\nFor your security, we never include account details in email. Sign in to view your statement.\n\nWoodgrove Bank Customer Care",
            true,
        ),
        msg(
            1046,
            2 * H + 5,
            addr("Sam Chen", "sam.chen@fabrikam.com"),
            "Re: Cabin weekend in March?",
            "Count me in! I can drive — the car fits five if nobody brings a giant suitcase this time (looking at you, Jordan).\n\nI'll check whether the place near the lake is still available for the 14th–16th.\n\n— Sam",
            true,
        ),
        {
            let mut m = msg(
                1045,
                3 * H + 40,
                addr("GitHub", "noreply@github.example"),
                "[gatekeep] Pull request #42 merged",
                "Merged #42 into master: \"Queue commands instead of disabling buttons while busy\".\n\n3 files changed, 118 insertions(+), 64 deletions(-)\n\nView it on GitHub.",
                false,
            );
            m.rules = vec!["Notifications"];
            m.low_priority = true;
            m
        },
        {
            let mut m = msg(
                1044,
                5 * H + 12,
                addr("Fourth Coffee", "hello@fourthcoffee.com"),
                "☕ Your free birthday drink is waiting",
                "Happy birthday month, Alex! Show this email at any Fourth Coffee location to get one handcrafted drink on us. Valid until the end of the month.\n\nUnsubscribe · Manage preferences",
                false,
            );
            m.rules = vec!["Newsletters"];
            m.low_priority = true;
            m
        },
        msg(
            1043,
            D + 2 * H,
            addr("Dr. Elena Vasquez", "frontdesk@lakesidedental.example"),
            "Appointment reminder: Tuesday 9:30",
            "Hi Alex,\n\nThis is a friendly reminder of your dental check-up on Tuesday at 9:30 AM with Dr. Vasquez.\n\nPlease reply to this email or call us if you need to reschedule.\n\nLakeside Dental",
            false,
        ),
        {
            let mut m = msg(
                1042,
                D + 6 * H,
                addr("Marcus Webb", "marcus.webb@contoso.com"),
                "Lunch & learn: intro to Rust on Friday",
                "Hey team,\n\nThis Friday at noon I'm running a short intro to Rust — ownership, borrowing and why the compiler yells at you. Pizza is on me.\n\nRoom 4B or on the video link.\n\nMarcus",
                false,
            );
            m.rules = vec!["Team"];
            m.highlight = true;
            m
        },
        {
            let mut m = msg(
                1041,
                D + 9 * H,
                addr("Northwind Traders", "orders@northwindtraders.com"),
                "Your order #NW-58213 has shipped",
                "Good news! Your order is on its way.\n\n  1 × Mechanical keyboard (brown switches)\n  1 × USB-C cable, 2 m\n\nEstimated delivery: Monday.\n\nTrack your package in your account.",
                false,
            );
            m.rules = vec!["Shopping"];
            m
        },
        msg(
            1040,
            2 * D + 3 * H,
            addr("Jordan Blake", "jordan.blake@adatum.com"),
            "Photos from Saturday 📸",
            "Finally sorted through them! There are some great ones from the hike — the one at the waterfall turned out amazing.\n\nShared album link is in the group chat. Let me know if you want the full-resolution versions.\n\nJ",
            false,
        ),
        {
            let mut m = msg(
                1039,
                2 * D + 8 * H,
                addr("Litware Weekly", "newsletter@litware.com"),
                "This week: 7 tools that changed how we ship",
                "In this issue: feature flags without the mess, why your CI is slow, and a deep dive into immediate-mode GUIs.\n\nRead online · Unsubscribe",
                false,
            );
            m.rules = vec!["Newsletters"];
            m.low_priority = true;
            m
        },
        msg(
            1038,
            3 * D + H,
            addr("Dana Okafor", "dana.okafor@contoso.com"),
            "Re: Offsite agenda draft",
            "Looks good to me. One suggestion: let's move the retro to the morning while everyone still has energy, and do the planning session after lunch.\n\nDana",
            false,
        ),
        msg(
            1037,
            4 * D + 4 * H,
            addr("City Library", "notices@citylibrary.example"),
            "2 items due in 3 days",
            "The following items are due soon:\n\n  • \"The Pragmatic Programmer\"\n  • \"A Philosophy of Software Design\"\n\nRenew online or visit any branch.",
            false,
        ),
        {
            let mut m = msg(
                1036,
                5 * D + 2 * H,
                addr("Proseware Cloud", "billing@proseware.com"),
                "Invoice for November — $12.00",
                "Thanks for using Proseware Cloud. Your invoice for November is attached.\n\nAmount: $12.00\nPayment method: card ending 4242\n\nNo action is needed.",
                false,
            );
            m.rules = vec!["Receipts"];
            m
        },
        {
            let mut m = msg(
                1035,
                6 * D + 5 * H,
                addr("Alpine Ski House", "news@alpineskihouse.com"),
                "Early-bird season passes end Sunday ⛷",
                "Save 30% on season passes when you buy before Sunday. Plus: new lifts on the north slope and longer night-skiing hours.\n\nUnsubscribe",
                false,
            );
            m.rules = vec!["Newsletters"];
            m.low_priority = true;
            m
        },
        msg(
            1034,
            8 * D,
            addr("Mom", "rivera.family@example.net"),
            "Sunday dinner",
            "Hi sweetie, are you coming on Sunday? Your aunt is bringing her famous lasagna. Let me know so I can set enough places.\n\nLove, Mom",
            false,
        ),
        {
            let mut m = msg(
                1033,
                9 * D + 3 * H,
                addr("Wingtip Toys", "shop@wingtiptoys.com"),
                "Your receipt from Wingtip Toys",
                "Thanks for shopping with us!\n\n  1 × Wooden train set — $34.99\n\nTotal: $34.99\n\nReturns accepted within 30 days.",
                false,
            );
            m.rules = vec!["Receipts"];
            m
        },
        msg(
            1032,
            11 * D,
            addr("Adventure Works", "trips@adventure-works.com"),
            "Booking confirmed: Lisbon, May 3–9",
            "Your trip is booked!\n\nFlight: departs 07:45, arrives 11:20\nHotel: Casa do Rio, 6 nights\nConfirmation code: AW-7Q2KX\n\nHave a wonderful trip.",
            false,
        ),
        msg(
            1031,
            13 * D,
            addr("Priya Natarajan", "priya.natarajan@contoso.com"),
            "Welcome aboard!",
            "Hi Alex,\n\nWelcome to the team! Your laptop should arrive on Monday. I've added you to the team channel and the weekly sync.\n\nSee you soon,\nPriya",
            false,
        ),
    ];
    v.sort_by_key(|m| std::cmp::Reverse(m.date));
    v
}
