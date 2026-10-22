// Big center container — the main container, always on: the frame around the
// zones, inside #mainGrid (ui/index.tsx), which also holds #chatScroll and,
// later, Shell use's #cliContainer. It holds the Skills stage
// (skills/skills_stage.tsx), shown while the zone is .active: skills.js
// toggles that, and chat_input.js drops .active on a new run and on Stop. React leaves that alone: a re-render
// writes only props whose value changed, so as long as className is rendered
// with this same value, the class list they change stays theirs. Same markup
// as big_center_container.html was
// (container/big_center_container/big_center_container.css styles it).
// Fires 'bigcenter:ready' once the zone is in the DOM, before paint, as the
// old loader did right after inserting it.
import { useLayoutEffect, useRef } from 'react';
import { SkillsStage } from '../../skills/skills_stage.tsx';

export function BigCenterContainer() {
    const zone = useRef<HTMLDivElement>(null);

    useLayoutEffect(() => {
        if (zone.current) document.dispatchEvent(new CustomEvent('bigcenter:ready'));
    }, []);

    return (
        <div className="container-zone big-center-zone" id="zoneBigCenter" aria-label="Big center" ref={zone}>
            <SkillsStage />
        </div>
    );
}
