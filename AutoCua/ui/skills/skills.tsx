// Skills — the composer icon, the last child of .agent-mode-wrap
// (agent_mode/agent_mode.tsx), just to the RIGHT of the Computer/Mobile use
// picker. A scroll "mark" whose writing scrolls upward on hover, with a
// floating SKILLS label above. This was skills.html, injected by skills.js;
// skills.js still wires the click, which opens the stage (skills_stage.tsx).
export function SkillsButton() {
    return (
        <button className="skills-btn" id="skillsBtn" type="button" aria-label="Skills" title="Skills">
            <svg className="skills-mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <defs>
                    <clipPath id="skillsSheet">
                        <rect x="4" y="6.25" width="16" height="11.5" />
                    </clipPath>
                </defs>

                {/* rolls */}
                <rect x="3" y="2.75" width="18" height="3.5" rx="1.75" />
                <rect x="3" y="17.75" width="18" height="3.5" rx="1.75" />

                {/* sheet edges */}
                <path d="M5.5 6.25 Q6.4 12 5.5 17.75" />
                <path d="M18.5 6.25 Q17.6 12 18.5 17.75" />

                {/* writing: two identical sets, so the loop is seamless */}
                <g clipPath="url(#skillsSheet)">
                    <g className="skills-lines">
                        <path d="M8.5 9.1 H15.5" />
                        <path d="M8.5 12 H15.5" />
                        <path d="M8.5 14.9 H13.2" />
                        <path d="M8.5 17.8 H15.5" />
                        <path d="M8.5 20.7 H15.5" />
                        <path d="M8.5 23.6 H13.2" />
                    </g>
                </g>
            </svg>
        </button>
    );
}
