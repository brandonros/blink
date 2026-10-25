/* Approximate FPATAN oracle with explicit input and output extended values. */
static long call(long n,long a,long b,long c) {long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
struct extended {unsigned long fraction;unsigned short exponent;} __attribute__((packed));
static const struct extended extended[]={
{0,0},{0,0x8000},{0x8000000000000000ul,0x3fff},{0x8000000000000000ul,0xbfff},
{0xaaaaaaaaaaaaaaaaul,0x3ffd},{0x8000000000000000ul,0x7fff},{0x8000000000000000ul,0xffff},
{0xc000000000000123ul,0x7fff},{0x8000000000000123ul,0xffff},{1,0},{0x8000000000000000ul,1},
{0xfffffffffffffffful,0x7ffe},{0x8000000000000000ul,0x3fbf},{1,1},{0,0x7fff},{0x7ffffffffffffffful,0x3fff},{0x8000000000000000ul,0},{0xfffffffffffffffful,0},{0x7ffffffffffffffful,0},{0,0x4000},{0,0xffff}};

struct record {struct extended x,y,result;unsigned short status,control,padding;unsigned sample;};
__attribute__((noreturn)) void _start(void) {
    for(unsigned mode=0;mode<12;mode++) for(unsigned pair=0;pair<441+2048;pair++) {
        struct record r={0};r.sample=pair;r.control=0x7f|((mode<4?0:mode<8?2:3)<<8)|((mode&3)<<10);
        if(pair<441){r.x=extended[pair%21];r.y=extended[pair/21];}
        else {
            unsigned long seed=(pair-440)*0x9e3779b97f4a7c15ul;
            seed ^= seed>>12;seed ^= seed<<25;seed ^= seed>>27;r.x.fraction=seed*0x2545f4914f6cdd1dul|0x8000000000000000ul;
            seed ^= seed>>12;seed ^= seed<<25;seed ^= seed>>27;r.y.fraction=seed*0x2545f4914f6cdd1dul|0x8000000000000000ul;
            if(pair<441+1024){r.x.exponent=(r.y.fraction>>32)&0xffff;r.y.exponent=(r.x.fraction>>32)&0xffff;}
            else {r.x.exponent=(0x3ffd+(pair&3))|((pair&4)?0x8000:0);r.y.exponent=(0x3ffd+((pair>>3)&3))|((pair&32)?0x8000:0);}
        }
        __asm__ volatile("fninit; fldcw %4; fldt %2; fldt %3; fpatan; fnstsw %1; fstpt %0":"=m"(r.result),"=m"(r.status):"m"(r.y),"m"(r.x),"m"(r.control):"st","st(1)","memory");
        if(call(1,1,(long)&r,sizeof r)!=sizeof r){call(231,1,0,0);__builtin_unreachable();}
    }
    call(231,0,0,0);__builtin_unreachable();
}
